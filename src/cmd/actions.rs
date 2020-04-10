/// Represents actions that are available while reading live output from a process
pub struct Actions {
    lines: Vec<String>,
}

impl<'a> Actions {
    pub(crate) fn new() -> Self {
        Actions { lines: Vec::new() }
    }

    pub(crate) fn next_input(&mut self, input: &str) {
        self.lines = vec![input.to_string()];
    }

    pub(crate) fn take_lines(&mut self) -> Vec<String> {
        std::mem::take(&mut self.lines)
    }

    /// Replace last read line with new_lines
    pub fn replace_with_lines(&mut self, new_lines: impl Iterator<Item = &'a str>) {
        self.lines = new_lines.map(|str| str.to_string()).collect();
    }

    /// Remove last read line from output
    pub fn remove_line(&mut self) {
        self.lines = Vec::new();
    }
}

#[cfg(test)]
mod test {
    use super::Actions;
    #[test]
    fn test_replace() {
        let mut actions = Actions::new();

        actions.next_input("lorem");
        actions.replace_with_lines("ipsum".split("\n"));
        assert_eq!(actions.take_lines(), vec!["ipsum"]);

        actions.next_input("lorem ipsum dolor");
        actions.replace_with_lines("lorem ipsum dolor".split(" "));
        assert_eq!(actions.take_lines(), vec!["lorem", "ipsum", "dolor"]);

        // assert last input is discarded
        assert_eq!(actions.take_lines(), Vec::<String>::new());
    }

    #[test]
    fn test_remove() {
        let mut actions = Actions::new();
        actions.next_input("lorem");
        actions.remove_line();
        assert_eq!(actions.take_lines(), Vec::<String>::new());
    }

    #[test]
    fn test_no_actions() {
        let mut actions = Actions::new();
        actions.next_input("lorem ipsum dolor");
        assert_eq!(actions.take_lines(), vec!["lorem ipsum dolor"]);
    }
}
