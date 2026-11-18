use std::str::FromStr;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Start,
    Help,
    CollectionList,
    CollectionNew(String),
    /// Raw argument; the handler parses it into a `Code`.
    CollectionJoin(String),
    Words,
    Remove(String),
    Stats,
    Period(String),
    Pause,
    Resume,
    Quiz,
    Skip,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("text is not a command")]
pub struct NotCommand;

impl FromStr for Command {
    type Err = NotCommand;

    fn from_str(text: &str) -> Result<Command, NotCommand> {
        let body = text.trim().strip_prefix('/').ok_or(NotCommand)?;
        let (head, arg) = match body.split_once(char::is_whitespace) {
            Some((head, arg)) => (head, arg.trim()),
            None => (body, ""),
        };
        // Drop the `@botname` mention that Telegram adds in group chats.
        let name = head.split_once('@').map_or(head, |(name, _)| name);
        let required = |make: fn(String) -> Command| {
            if arg.is_empty() {
                Command::Unknown
            } else {
                make(arg.to_owned())
            }
        };
        Ok(match name.to_lowercase().as_str() {
            "start" => Command::Start,
            "help" => Command::Help,
            "collection" => collection(arg),
            "words" => Command::Words,
            "remove" => required(Command::Remove),
            "stats" => Command::Stats,
            "period" => required(Command::Period),
            "pause" => Command::Pause,
            "resume" => Command::Resume,
            "quiz" => Command::Quiz,
            "skip" => Command::Skip,
            _ => Command::Unknown,
        })
    }
}

fn collection(arg: &str) -> Command {
    if arg.is_empty() {
        return Command::CollectionList;
    }
    match arg.split_once(char::is_whitespace) {
        Some((word, title)) if word.eq_ignore_ascii_case("new") && !title.trim().is_empty() => {
            Command::CollectionNew(title.trim().to_owned())
        }
        None if arg.eq_ignore_ascii_case("new") => Command::Unknown,
        _ => Command::CollectionJoin(arg.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Command, NotCommand> {
        text.parse::<Command>()
    }

    #[test]
    fn parses_collection_forms() {
        assert_eq!(parse("/collection"), Ok(Command::CollectionList));
        assert_eq!(
            parse("/collection new Греческий А2"),
            Ok(Command::CollectionNew("Греческий А2".into()))
        );
        assert_eq!(
            parse("/collection K7QM-3XPA"),
            Ok(Command::CollectionJoin("K7QM-3XPA".into()))
        );
    }

    #[test]
    fn collection_new_is_case_insensitive() {
        assert_eq!(
            parse("/collection New Foo"),
            Ok(Command::CollectionNew("Foo".into()))
        );
        assert_eq!(parse("/collection NEW"), Ok(Command::Unknown));
    }

    #[test]
    fn strips_bot_mention() {
        assert_eq!(parse("/quiz@wordropbot"), Ok(Command::Quiz));
    }

    #[test]
    fn plain_text_is_not_command() {
        assert_eq!(parse("σπίτι дом"), Err(NotCommand));
    }

    #[test]
    fn unknown_slash_is_never_imported() {
        assert_eq!(parse("/foo bar"), Ok(Command::Unknown));
    }

    #[test]
    fn missing_required_argument_is_unknown() {
        for text in [
            "/remove",
            "/remove   ",
            "/period",
            "/collection new",
            "/collection new  ",
        ] {
            assert_eq!(parse(text), Ok(Command::Unknown), "{text}");
        }
    }

    #[test]
    fn arguments_are_trimmed() {
        assert_eq!(
            parse("/remove   σπίτι  "),
            Ok(Command::Remove("σπίτι".into()))
        );
        assert_eq!(parse("/period@bot 2h"), Ok(Command::Period("2h".into())));
    }
}
