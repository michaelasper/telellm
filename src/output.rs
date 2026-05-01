use crate::config::OutputConfig;
use std::collections::HashSet;

pub fn prompt_instruction(config: &OutputConfig) -> Option<String> {
    config.enabled.then(|| {
        format!(
            "File output: when asked to create a file for Telegram, create `/workspace/{}` if needed, write the file there using a filename without spaces, then mention each sendable file in your final answer as `@{}/filename.ext`. telellm will upload mentioned files back to Telegram.",
            config.workspace_dir, config.workspace_dir
        )
    })
}

pub fn extract_file_refs(text: &str, workspace_dir: &str, max_files: usize) -> Vec<String> {
    let prefix = format!("@{}/", workspace_dir.trim_matches('/'));
    let mut refs = Vec::new();
    let mut seen = HashSet::new();
    let mut offset = 0;

    while refs.len() < max_files {
        let Some(index) = text[offset..].find(&prefix) else {
            break;
        };
        let start = offset + index + 1;
        let tail = &text[start..];
        let len = tail
            .char_indices()
            .find_map(|(idx, ch)| (!is_file_ref_char(ch)).then_some(idx))
            .unwrap_or(tail.len());
        let candidate = trim_trailing_file_ref_punctuation(&tail[..len]);
        if is_valid_file_ref(candidate, workspace_dir) && seen.insert(candidate.to_owned()) {
            refs.push(candidate.to_owned());
        }
        offset = start + len;
    }

    refs
}

pub fn file_name_for_workspace_path(workspace_path: &str) -> String {
    workspace_path
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or("telegram-output")
        .to_owned()
}

fn is_valid_file_ref(candidate: &str, workspace_dir: &str) -> bool {
    let prefix = format!("{}/", workspace_dir.trim_matches('/'));
    candidate.starts_with(&prefix) && candidate.len() > prefix.len()
}

fn is_file_ref_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '/' | '.' | '_' | '-')
}

fn trim_trailing_file_ref_punctuation(value: &str) -> &str {
    value.trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']'])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_file_refs_should_find_unique_workspace_refs() {
        let refs = extract_file_refs(
            "Created `@telegram_outputs/report.pdf` and @telegram_outputs/chart.png.",
            "telegram_outputs",
            4,
        );

        assert_eq!(
            refs,
            vec![
                "telegram_outputs/report.pdf".to_owned(),
                "telegram_outputs/chart.png".to_owned()
            ]
        );
    }

    #[test]
    fn extract_file_refs_should_ignore_other_dirs_and_limit_results() {
        let refs = extract_file_refs(
            "@other/file.pdf @telegram_outputs/a.pdf @telegram_outputs/b.pdf",
            "telegram_outputs",
            1,
        );

        assert_eq!(refs, vec!["telegram_outputs/a.pdf".to_owned()]);
    }

    #[test]
    fn file_name_for_workspace_path_should_use_last_component() {
        assert_eq!(
            file_name_for_workspace_path("telegram_outputs/reports/a.pdf"),
            "a.pdf"
        );
    }
}
