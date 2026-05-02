use crate::bot::message::IncomingMessage;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SummaryPromptError {
    #[error("no recent chat is available to summarize")]
    NoRecentChat,
}

pub fn build_summary_prompt(
    system_prompt: &str,
    recent_messages: &[IncomingMessage],
    focus: Option<&str>,
) -> Result<String, SummaryPromptError> {
    let useful: Vec<_> = recent_messages
        .iter()
        .filter(|message| !is_summary_command_text(&message.text))
        .filter(|message| !message.text.trim().is_empty() || !message.context_notes.is_empty())
        .collect();
    if useful.is_empty() {
        return Err(SummaryPromptError::NoRecentChat);
    }

    let mut prompt = String::new();
    prompt.push_str(system_prompt.trim());
    prompt.push_str(
        "\n\nWrite a concise catch-up recap of the recent Telegram chat for someone who is joining late.",
    );
    if let Some(focus) = focus.filter(|value| !value.trim().is_empty()) {
        prompt.push_str("\nFocus: ");
        prompt.push_str(focus.trim());
    }
    prompt.push_str("\n\nRecent chat:\n");
    for message in useful {
        let name = message.from_name.as_deref().unwrap_or("unknown");
        prompt.push_str("- ");
        prompt.push_str(name);
        prompt.push_str(": ");
        let text = message.text.trim();
        prompt.push_str(if text.is_empty() { "(no text)" } else { text });
        prompt.push('\n');
        for note in &message.context_notes {
            prompt.push_str("  context: ");
            prompt.push_str(note);
            prompt.push('\n');
        }
    }
    Ok(prompt)
}

fn is_summary_command_text(text: &str) -> bool {
    let Some(first_token) = text.trim().split_whitespace().next() else {
        return false;
    };
    if !first_token.starts_with('/') {
        return false;
    }
    let command_name = first_token
        .trim_start_matches('/')
        .split('@')
        .next()
        .unwrap_or_default();
    command_name.eq_ignore_ascii_case("summarize")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        bot::message::IncomingMessage,
        ids::{ChatId, MessageId, UserId},
    };

    fn msg(id: i32, text: &str) -> IncomingMessage {
        IncomingMessage {
            chat_id: ChatId(1),
            message_id: MessageId(id),
            from: Some(UserId(2)),
            from_name: Some("Mike".to_owned()),
            text: text.to_owned(),
            attachments: Vec::new(),
            context_notes: Vec::new(),
            reply_to_bot: false,
            reply_to: None,
            private_chat: false,
        }
    }

    #[test]
    fn summary_prompt_should_include_recent_chat_and_focus() {
        let prompt = build_summary_prompt(
            "System prompt.",
            &[msg(1, "first topic"), msg(2, "deploy failed")],
            Some("deploy"),
        )
        .expect("prompt");

        assert!(prompt.contains("catch-up recap"));
        assert!(prompt.contains("deploy failed"));
        assert!(prompt.contains("Focus: deploy"));
    }

    #[test]
    fn summary_prompt_should_reject_empty_recent_chat() {
        let err = build_summary_prompt("System prompt.", &[], None).expect_err("empty");
        assert_eq!(err, SummaryPromptError::NoRecentChat);
    }

    #[test]
    fn summary_prompt_should_reject_command_only_recent_chat_for_parser_compatible_forms() {
        let err = build_summary_prompt(
            "System prompt.",
            &[
                msg(1, "/Summarize"),
                msg(2, "/SUMMARIZE@telellm_bot"),
                msg(3, "/summarize@telellm_bot deploy"),
            ],
            None,
        )
        .expect_err("command-only chat");

        assert_eq!(err, SummaryPromptError::NoRecentChat);
    }
}
