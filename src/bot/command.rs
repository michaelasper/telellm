#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BotCommand {
    Help,
    Status,
    Reset { clear_workspace: bool },
    Restart,
    Rebuild { clear_workspace: bool },
    Memory,
    Remember { content: String },
    Forget { target: ForgetTarget },
    Voice { target: VoiceTarget },
    Summarize { focus: Option<String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForgetTarget {
    All,
    Query(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceTarget {
    On,
    Off,
    Status,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandDefinition {
    pub command: &'static str,
    pub description: &'static str,
}

pub const COMMAND_DEFINITIONS: &[CommandDefinition] = &[
    CommandDefinition {
        command: "help",
        description: "Show command help.",
    },
    CommandDefinition {
        command: "status",
        description: "Show this chat's Codex runtime status.",
    },
    CommandDefinition {
        command: "reset",
        description: "Reset this chat's Codex session.",
    },
    CommandDefinition {
        command: "restart",
        description: "Restart this chat's sandbox.",
    },
    CommandDefinition {
        command: "rebuild",
        description: "Recreate this chat's sandbox container.",
    },
    CommandDefinition {
        command: "memory",
        description: "List durable group memories.",
    },
    CommandDefinition {
        command: "remember",
        description: "Store a durable group memory.",
    },
    CommandDefinition {
        command: "forget",
        description: "Forget durable group memories.",
    },
    CommandDefinition {
        command: "voice",
        description: "Manage spoken replies for this chat.",
    },
    CommandDefinition {
        command: "summarize",
        description: "Summarize recent chat for catching up.",
    },
];

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CommandParseError {
    #[error("unknown command `{0}`")]
    Unknown(String),
    #[error("missing argument for `{0}`")]
    MissingArgument(&'static str),
}

impl BotCommand {
    pub fn parse(input: &str, bot_username: &str) -> Result<Option<Self>, CommandParseError> {
        let trimmed = input.trim();
        let Some(first) = trimmed.split_whitespace().next() else {
            return Ok(None);
        };
        if !first.starts_with('/') {
            return Ok(None);
        }

        let command_name = first
            .trim_start_matches('/')
            .split('@')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();

        if let Some((_, addressed_to)) = first.split_once('@')
            && !addressed_to.eq_ignore_ascii_case(bot_username)
        {
            return Ok(None);
        }

        let rest = trimmed[first.len()..].trim();
        match command_name.as_str() {
            "help" => Ok(Some(Self::Help)),
            "status" => Ok(Some(Self::Status)),
            "reset" => Ok(Some(Self::Reset {
                clear_workspace: rest.contains("--clear-workspace"),
            })),
            "restart" => Ok(Some(Self::Restart)),
            "rebuild" => Ok(Some(Self::Rebuild {
                clear_workspace: rest.contains("--clear-workspace"),
            })),
            "memory" => Ok(Some(Self::Memory)),
            "remember" => {
                if rest.is_empty() {
                    return Err(CommandParseError::MissingArgument("/remember"));
                }
                Ok(Some(Self::Remember {
                    content: rest.to_owned(),
                }))
            }
            "forget" => {
                if rest.is_empty() {
                    return Err(CommandParseError::MissingArgument("/forget"));
                }
                if rest == "all" {
                    Ok(Some(Self::Forget {
                        target: ForgetTarget::All,
                    }))
                } else {
                    Ok(Some(Self::Forget {
                        target: ForgetTarget::Query(rest.to_owned()),
                    }))
                }
            }
            "voice" => {
                let target = match rest.to_ascii_lowercase().as_str() {
                    "" | "status" => VoiceTarget::Status,
                    "on" => VoiceTarget::On,
                    "off" => VoiceTarget::Off,
                    _ => return Err(CommandParseError::Unknown(format!("voice {rest}"))),
                };
                Ok(Some(Self::Voice { target }))
            }
            "summarize" => Ok(Some(Self::Summarize {
                focus: (!rest.is_empty()).then(|| rest.to_owned()),
            })),
            other => Err(CommandParseError::Unknown(other.to_owned())),
        }
    }
}

pub fn command_definitions() -> &'static [CommandDefinition] {
    COMMAND_DEFINITIONS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_should_return_none_for_plain_text() {
        let parsed = BotCommand::parse("hello", "telellm_bot").expect("parse should succeed");

        assert_eq!(parsed, None);
    }

    #[test]
    fn parse_should_accept_command_addressed_to_this_bot() {
        let parsed =
            BotCommand::parse("/status@telellm_bot", "telellm_bot").expect("parse should succeed");

        assert_eq!(parsed, Some(BotCommand::Status));
    }

    #[test]
    fn parse_should_ignore_command_addressed_to_another_bot() {
        let parsed =
            BotCommand::parse("/status@other_bot", "telellm_bot").expect("parse should succeed");

        assert_eq!(parsed, None);
    }

    #[test]
    fn parse_should_capture_forget_query() {
        let parsed = BotCommand::parse("/forget Mike hates cilantro", "telellm_bot")
            .expect("parse should succeed");

        assert_eq!(
            parsed,
            Some(BotCommand::Forget {
                target: ForgetTarget::Query("Mike hates cilantro".to_owned())
            })
        );
    }

    #[test]
    fn parse_should_capture_remember() {
        let parsed = BotCommand::parse("/remember Mike likes short answers", "telellm_bot")
            .expect("parse should succeed");

        assert_eq!(
            parsed,
            Some(BotCommand::Remember {
                content: "Mike likes short answers".to_owned()
            })
        );
    }

    #[test]
    fn parse_should_capture_voice_commands() {
        assert_eq!(
            BotCommand::parse("/voice on", "telellm_bot").expect("parse"),
            Some(BotCommand::Voice {
                target: VoiceTarget::On,
            })
        );
        assert_eq!(
            BotCommand::parse("/voice off", "telellm_bot").expect("parse"),
            Some(BotCommand::Voice {
                target: VoiceTarget::Off,
            })
        );
        assert_eq!(
            BotCommand::parse("/voice status", "telellm_bot").expect("parse"),
            Some(BotCommand::Voice {
                target: VoiceTarget::Status,
            })
        );
        assert_eq!(
            BotCommand::parse("/voice", "telellm_bot").expect("parse"),
            Some(BotCommand::Voice {
                target: VoiceTarget::Status,
            })
        );
    }

    #[test]
    fn parse_should_capture_summarize_commands() {
        assert_eq!(
            BotCommand::parse("/summarize", "telellm_bot").expect("parse"),
            Some(BotCommand::Summarize { focus: None })
        );
        assert_eq!(
            BotCommand::parse("/summarize the deploy", "telellm_bot").expect("parse"),
            Some(BotCommand::Summarize {
                focus: Some("the deploy".to_owned()),
            })
        );
    }

    #[test]
    fn command_definitions_should_include_supported_commands() {
        let commands: Vec<_> = command_definitions()
            .iter()
            .map(|definition| definition.command)
            .collect();

        assert_eq!(
            commands,
            vec![
                "help",
                "status",
                "reset",
                "restart",
                "rebuild",
                "memory",
                "remember",
                "forget",
                "voice",
                "summarize"
            ]
        );
    }

    #[test]
    fn command_definitions_should_be_valid_for_telegram_menu() {
        for definition in command_definitions() {
            assert!(!definition.command.starts_with('/'));
            assert!(!definition.command.is_empty());
            assert!(definition.command.len() <= 32);
            assert!(definition.description.len() >= 3);
            assert!(definition.description.len() <= 256);
            assert!(
                definition
                    .command
                    .chars()
                    .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
            );
        }
    }
}
