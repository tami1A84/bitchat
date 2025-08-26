use std::fmt;

#[derive(Debug, PartialEq)]
pub enum Command {
    Msg {
        nickname: String,
        message: Option<String>,
    },
    Who,
    Clear,
    Hug {
        nickname: String,
    },
    Slap {
        nickname: String,
    },
    Block {
        nickname: String,
    },
    Unblock {
        nickname: String,
    },
    Fav {
        nickname: String,
    },
    Unfav {
        nickname: String,
    },
    Help,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ParseError(String);

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ParseError {}

pub fn process_command(input: &str) -> Result<Command, ParseError> {
    let mut parts = input.trim_start_matches('/').splitn(2, ' ');
    let command = parts.next().unwrap_or("").to_lowercase();
    let args = parts.next().unwrap_or("").trim();

    match command.as_str() {
        "m" | "msg" => {
            let mut arg_parts = args.splitn(2, ' ');
            let nickname = arg_parts.next().ok_or_else(|| ParseError("usage: /msg @nickname [message]".to_string()))?;
            let message = arg_parts.next().map(|s| s.to_string());
            Ok(Command::Msg {
                nickname: nickname.trim_start_matches('@').to_string(),
                message,
            })
        }
        "w" | "who" => Ok(Command::Who),
        "clear" => Ok(Command::Clear),
        "hug" => {
            if args.is_empty() {
                return Err(ParseError("usage: /hug <nickname>".to_string()));
            }
            Ok(Command::Hug {
                nickname: args.trim_start_matches('@').to_string(),
            })
        }
        "slap" => {
            if args.is_empty() {
                return Err(ParseError("usage: /slap <nickname>".to_string()));
            }
            Ok(Command::Slap {
                nickname: args.trim_start_matches('@').to_string(),
            })
        }
        "block" => {
            if args.is_empty() {
                return Err(ParseError("usage: /block <nickname>".to_string()));
            }
            Ok(Command::Block {
                nickname: args.trim_start_matches('@').to_string(),
            })
        }
        "unblock" => {
            if args.is_empty() {
                return Err(ParseError("usage: /unblock <nickname>".to_string()));
            }
            Ok(Command::Unblock {
                nickname: args.trim_start_matches('@').to_string(),
            })
        }
        "fav" => {
            if args.is_empty() {
                return Err(ParseError("usage: /fav <nickname>".to_string()));
            }
            Ok(Command::Fav {
                nickname: args.trim_start_matches('@').to_string(),
            })
        }
        "unfav" => {
            if args.is_empty() {
                return Err(ParseError("usage: /unfav <nickname>".to_string()));
            }
            Ok(Command::Unfav {
                nickname: args.trim_start_matches('@').to_string(),
            })
        }
        "h" | "help" => Ok(Command::Help),
        _ => Err(ParseError(format!("unknown command: /{}", command))),
    }
}
