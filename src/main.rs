use git2::Repository;
use regex::Regex;
use std::collections::BTreeMap;
#[macro_use]
extern crate lazy_static;

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

    let reports = revwalk
        .filter_map(|item| item.ok())
        .filter_map(|rev| repo.find_commit(rev).ok())
        .filter_map(|commit| commit.message().map(|i| i.to_string()))
        .filter_map(|message| parse_report(&message));

    let mut aggregator = ReportAggregator::new();
    for report in reports {
        aggregator.add_report(report);
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
                writeln!(&mut out, "### {}", k)?;
            }

            if v[FEAT_TYPE].len() > 0 {
                writeln!(out, "#### Features")?;
                for report in &v[FEAT_TYPE] {
                    report.print(&mut out)?;
                }
            }

            if v[FIX_TYPE].len() > 0 {
                writeln!(out, "#### Fixes");
                for report in &v[FIX_TYPE] {
                    report.print(&mut out)?;
                }
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
    #[test]
    fn it_should_parse_reports() {
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

        for (i, expected) in test_table.iter().enumerate() {
            let mut d = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            d.push(format!("test_assets/commit_messages/{}.txt", i + 1));

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
    }
}
