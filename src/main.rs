use git2::Repository;
use regex::Regex;
use std::collections::BTreeMap;
#[macro_use]
extern crate lazy_static;
extern crate crossbeam;
#[macro_use]
use crossbeam::crossbeam_channel;

fn main() -> Result<(), Box<std::error::Error>> {
    let current_dir = std::env::current_dir()?;
    let repo = Repository::open(current_dir)?;
    let mut revwalk = repo.revwalk()?;
    let tags = repo.tag_names(None)?;
    let mut reports: Vec<git2::Commit> = tags
        .iter()
        .filter(|possible_tag| possible_tag.is_some())
        .map(|t| t.unwrap())
        .map(|raw_tag| {
            repo.revparse_single(raw_tag)
                .expect("unable to find reference for tag")
                .as_commit()
                .unwrap()
                .to_owned()
        })
        .collect();
    reports.sort_by(|a, b| a.time().seconds().cmp(&b.time().seconds()));

    let possible_latest_tag = reports.last();
    if possible_latest_tag.is_none() {
        println!("no tags found. exiting");
        return Ok(());
    }
    let latest_tag = possible_latest_tag.unwrap();
    revwalk.hide(latest_tag.id())?;
    revwalk.push_head()?;

    let (oid_send, oid_receive) = crossbeam_channel::unbounded::<git2::Oid>();
    let (report_send, report_receive) = crossbeam_channel::unbounded::<Report>();
    let mut num_items = 0;
    let reports = revwalk
        .filter_map(|item| item.ok())
        .enumerate()
        .for_each(|(index, item)| {
            oid_send.send(item).expect("unable to send oid to workers");
            num_items = index;
        });
    /* .filter_map(|rev| repo.find_commit(rev).ok())
    .filter_map(|commit| commit.message().map(|i| i.to_string()))
    .filter_map(|message| parse_report(&message));*/

    let mut aggregator = ReportAggregator::new();
    for (index, report) in report_receive.iter().enumerate() {
        aggregator.add_report(report);
        if index == num_items {
            break;
        }
    }

    aggregator.print(std::io::stdout())?;

    Ok(())
}
fn parse_report(raw_input: &str) -> Option<Report> {
    lazy_static! {
        static ref SPLITTER: Regex = Regex::new(r"\n(\n|\s+\n)+").unwrap();
    }

    let mut split = SPLITTER.split(raw_input);

    let raw_head_line = split.next().unwrap_or("");

    let mut headline_parts: Vec<&str> = raw_head_line.split(":").collect();

    if headline_parts.len() < 2 {
        return None;
    }

    let raw_context_and_type = headline_parts.remove(0);

    let mut type_parts: Vec<&str> = raw_context_and_type.split("(").collect();
    let commit_type = type_parts.remove(0);

    let context = if !type_parts.is_empty() {
        type_parts.remove(0).replace(")", "")
    } else {
        String::new()
    };
    let headline = headline_parts.join(":").trim().to_string();

    let mut result = match &commit_type.to_lowercase()[..] {
        "feat" | "feature" => Report {
            header: headline,
            commit_type: FEAT_TYPE,
            description: None,
            context: context,
            related_issues: Vec::new(),
            solved_issues: Vec::new(),
            breaking_changes: Vec::new(),
        },
        "fix" => Report {
            header: headline,
            commit_type: FIX_TYPE,
            description: None,
            context: context,
            related_issues: Vec::new(),
            solved_issues: Vec::new(),
            breaking_changes: Vec::new(),
        },
        _ => return None,
    };

    for mut part in split {
        part = part.trim();
        if part == "" {
            continue;
        }
        if part.to_lowercase().starts_with("solves:\n") {
            result.solved_issues = parse_array(part);
        } else if part.to_lowercase().starts_with("related:\n") {
            result.related_issues = parse_array(part);
        } else if part.to_lowercase().starts_with("breaking_changes:\n")
            || part.to_lowercase().starts_with("breaking changes:\n")
        {
            result.breaking_changes = parse_array(part);
        } else {
            result.description = Some(part.to_string());
        }
    }

    Some(result)
}

struct Worker<'a> {
    oid_receiver: crossbeam_channel::Receiver<git2::Oid>,
    commit_message_receiver: crossbeam_channel::Receiver<String>,
    commit_message_sender: crossbeam_channel::Sender<String>,
    report_sender: crossbeam_channel::Sender<Option<Report>>,
    repo: &'a git2::Repository,
}

impl<'a> Worker<'a> {
    fn run(&self) {
        crossbeam_channel::select! {
            recv(self.oid_receiver) -> possible_oid => {
                let oid = possible_oid.expect("unable to receive oid");
                let result = self.process_commit(oid);
                if result.is_err() {
                    panic!("error while commit lookup: {}",result.err().unwrap());
                }
            }
        };
    }

    fn process_commit(&self, oid: git2::Oid) -> Result<(), Box<std::error::Error>> {
        let commit = self.repo.find_commit(oid)?;
        let message = commit.message().unwrap_or("");
        if message == "" {
            return Ok(());
        }
        self.commit_message_sender
            .send(commit.message().unwrap_or("").to_string())?;
        Ok(())
    }
}

fn parse_array(input: &str) -> Vec<String> {
    lazy_static! {
        static ref CLEANER: Regex = Regex::new(r"\s+-\s+").unwrap();
    }
    CLEANER
        .split(input)
        .skip(1)
        .map(|i| i.to_string())
        .collect()
}

struct Report {
    header: String,
    description: Option<String>,
    context: String,
    commit_type: usize,
    related_issues: Vec<String>,
    solved_issues: Vec<String>,
    breaking_changes: Vec<String>,
}

impl Report {
    fn print(&self, mut out: impl std::io::Write) -> std::io::Result<()> {
        writeln!(&mut out, "{}\n", self.header)?;
        if self.description.is_some() {
            writeln!(&mut out, "{}", self.description.clone().unwrap())?;
        }
        Ok(())
    }
}

struct ReportAggregator {
    reports: BTreeMap<String, [Vec<Report>; 2]>,
    breaking_changes: Vec<String>,
}

impl ReportAggregator {
    fn new() -> Self {
        ReportAggregator {
            reports: BTreeMap::new(),
            breaking_changes: Vec::new(),
        }
    }

    fn add_report(&mut self, report: Report) {
        for bc in &report.breaking_changes {
            self.breaking_changes.push(bc.clone());
        }
        self.reports
            .entry(report.context.clone())
            .or_insert([Vec::new(), Vec::new()])[report.commit_type]
            .push(report);
    }

    fn print(&self, mut out: impl std::io::Write) -> std::io::Result<()> {
        for (k, v) in &self.reports {
            if v[FIX_TYPE].len() > 0 || v[FEAT_TYPE].len() > 0 {
                if k != "" {
                    writeln!(&mut out, "### {}\n", k)?;
                }
            }

            if v[FEAT_TYPE].len() > 0 {
                if k == "" {
                    writeln!(out, "### General Features\n")?;
                } else {
                    writeln!(out, "#### Features\n")?;
                }
                for report in &v[FEAT_TYPE] {
                    report.print(&mut out)?;
                }
                writeln!(out)?;
            }

            if v[FIX_TYPE].len() > 0 {
                if k == "" {
                    writeln!(out, "### General Fixes\n")?;
                } else {
                    writeln!(out, "#### Fixes\n")?;
                }
                for report in &v[FIX_TYPE] {
                    report.print(&mut out)?;
                }
                writeln!(out)?;
            }
        }
        if self.breaking_changes.len() > 0 {
            writeln!(out, "### BREAKING CHANGES\n")?;
            for bc in &self.breaking_changes {
                writeln!(out, "{}\n", bc)?;
            }
        }
        Ok(())
    }
}

const FIX_TYPE: usize = 1;
const FEAT_TYPE: usize = 0;

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;
    use std::io::prelude::*;

    #[test]
    fn it_should_parse_reports() {
        let update_golden = std::env::var("UPDATE_GOLDEN");
        let test_table = vec![Report {
            header: "Insert some stuff".to_string(),
            description: Some(
                "This commit will insert some stuff. \nIt is intendet to test if this works or not."
                    .to_string(),
            ),
            context: "cmd/update".to_string(),
            commit_type: FEAT_TYPE,
            related_issues: vec!["foo".to_string(), "bar".to_string()],
            solved_issues: vec!["hallo".to_string(), "welt".to_string()],
            breaking_changes: vec!["bla".to_string(), "blubb".to_string()],
        },
        Report {
            header: "Some fix".to_string(),
            description:None,
            context: String::new(),
            commit_type: FIX_TYPE,
            related_issues: vec![],
            solved_issues: vec![],
            breaking_changes: vec![],
        },
         Report {
            header: "Fix something".to_string(),
            description:None,
            context: String::new(),
            commit_type: FIX_TYPE,
            related_issues: vec![],
            solved_issues: vec![],
            breaking_changes: vec!["break something".to_string(),"break some real long thing\nthat wraps arround two lines".to_string()],
        },
        ];

        let mut aggregator = ReportAggregator::new();
        let mut test_assets_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        test_assets_path.push("test_assets");
        for (i, expected) in test_table.iter().enumerate() {
            let mut d = test_assets_path.clone();
            d.push(format!("commit_messages/{}.txt", i + 1));

            let commit_message =
                std::fs::read_to_string(d).expect("unable to read commit message file");

            let possible_report = parse_report(&commit_message);
            assert!(possible_report.is_some());
            let report = possible_report.unwrap();
            assert_eq!(report.header, expected.header);
            assert_eq!(report.description, expected.description);
            assert_eq!(expected.commit_type, report.commit_type);
            assert_eq!(expected.context, report.context);
            assert_eq!(expected.solved_issues, report.solved_issues);
            assert_eq!(expected.related_issues, report.related_issues);
            assert_eq!(expected.breaking_changes, report.breaking_changes);
        }

        for rep in test_table {
            aggregator.add_report(rep);
        }
        let mut change_log_path = test_assets_path.clone();
        change_log_path.push("change_logs/1.txt");
        if update_golden.is_ok() {
            let f =
                std::fs::File::create(&change_log_path).expect("unable to create change logs file");

            let result = aggregator.print(f);
            assert!(result.is_ok());
            return;
        }

        let mut output = Vec::new();
        let result = aggregator.print(&mut output);
        match result {
            Ok(_) => {}
            Err(e) => {
                let desc = e.description().to_string();
                panic!(desc);
            }
        }

        let mut f =
            std::fs::File::open(&change_log_path).expect("unable to open ch ange logs file");
        let mut expected_content = String::new();
        f.read_to_string(&mut expected_content)
            .expect("unable to read changelog file");

        let actual = String::from_utf8_lossy(&output).into_owned();
        assert_eq!(expected_content, actual);
    }
}
