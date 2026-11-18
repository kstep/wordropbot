#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Command, NotACommand> {
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
    fn strips_bot_mention() {
        assert_eq!(parse("/quiz@wordropbot"), Ok(Command::Quiz));
    }

    #[test]
    fn plain_text_is_not_a_command() {
        assert_eq!(parse("σπίτι дом"), Err(NotACommand));
    }

    #[test]
    fn unknown_slash_is_never_imported() {
        assert_eq!(parse("/foo bar"), Ok(Command::Unknown));
    }

    #[test]
    fn missing_required_argument_is_unknown() {
        for text in ["/remove", "/remove   ", "/period", "/collection new", "/collection new  "] {
            assert_eq!(parse(text), Ok(Command::Unknown), "{text}");
        }
    }

    #[test]
    fn arguments_are_trimmed() {
        assert_eq!(parse("/remove   σπίτι  "), Ok(Command::Remove("σπίτι".into())));
        assert_eq!(parse("/period@bot 2h"), Ok(Command::Period("2h".into())));
    }
}
