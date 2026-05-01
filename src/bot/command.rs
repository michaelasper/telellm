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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForgetTarget {
    All,
    Query(String),
}

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
            other => Err(CommandParseError::Unknown(other.to_owned())),
        }
    }
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
}
