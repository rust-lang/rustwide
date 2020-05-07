use std::default::Default;

#[cfg_attr(test, derive(Debug, PartialEq))]
pub(super) enum InnerState {
    Removed,
    Original,
    Replaced(Vec<String>),
}

impl Default for InnerState {
    fn default() -> Self {
        InnerState::Original
    }
}

/// Represents actions that are available while reading live output from a process.
///
/// This will be available inside the function you provide to [`Command::process_lines`](struct.Command.html#method.process_lines)
pub struct ProcessLinesActions {
    state: InnerState,
}

impl<'a> ProcessLinesActions {
    pub(super) fn new() -> Self {
        ProcessLinesActions {
            state: InnerState::default(),
        }
    }

    pub(super) fn take_lines(&mut self) -> InnerState {
        std::mem::take(&mut self.state)
    }

    /// Replace last read line from output with the lines provided.
    ///
    /// The new lines will be logged instead of the original line.
    pub fn replace_with_lines(&mut self, new_lines: impl Iterator<Item = &'a str>) {
        self.state = InnerState::Replaced(new_lines.map(|str| str.to_string()).collect());
    }

    /// Remove last read line from output.
    ///
    /// This means that the line will not be logged.
    pub fn remove_line(&mut self) {
        self.state = InnerState::Removed;
    }
}

#[cfg(test)]
mod test {
    use super::InnerState;
    use super::ProcessLinesActions;
    #[test]
    fn test_replace() {
        let mut actions = ProcessLinesActions::new();

        actions.replace_with_lines("ipsum".split("\n"));
        assert_eq!(
            actions.take_lines(),
            InnerState::Replaced(vec!["ipsum".to_string()])
        );

        actions.replace_with_lines("lorem ipsum dolor".split(" "));
        assert_eq!(
            actions.take_lines(),
            InnerState::Replaced(vec![
                "lorem".to_string(),
                "ipsum".to_string(),
                "dolor".to_string()
            ])
        );

        // assert last input is discarded
        assert_eq!(actions.take_lines(), InnerState::Original);
    }

    #[test]
    fn test_remove() {
        let mut actions = ProcessLinesActions::new();
        actions.remove_line();
        assert_eq!(actions.take_lines(), InnerState::Removed);
    }

    #[test]
    fn test_no_actions() {
        let mut actions = ProcessLinesActions::new();
        assert_eq!(actions.take_lines(), InnerState::Original);
    }
}
