use super::{CommandLike, Invocation, SourceUnit, Word};

/// IR walker. Default impls recurse into children; override the methods
/// you care about. The safety AST scan and the resolver loop both consume
/// the IR through this trait.
pub trait Visitor {
    fn visit_unit(&mut self, unit: &SourceUnit) {
        walk_unit(self, unit);
    }

    fn visit_command(&mut self, cmd: &CommandLike) {
        walk_command(self, cmd);
    }

    fn visit_invocation(&mut self, inv: &Invocation) {
        walk_invocation(self, inv);
    }

    fn visit_word(&mut self, _word: &Word) {
        // Leaf by default. Override to inspect WordPieces.
    }
}

pub fn walk_unit<V: Visitor + ?Sized>(v: &mut V, unit: &SourceUnit) {
    for cmd in &unit.commands {
        v.visit_command(cmd);
    }
}

pub fn walk_command<V: Visitor + ?Sized>(v: &mut V, cmd: &CommandLike) {
    match cmd {
        CommandLike::Simple(inv) => v.visit_invocation(inv),
        CommandLike::Function { body, .. } => v.visit_unit(body),
        CommandLike::Source { target, .. } => v.visit_word(target),
        CommandLike::Alias { definition, .. } => v.visit_word(definition),
    }
}

pub fn walk_invocation<V: Visitor + ?Sized>(v: &mut V, inv: &Invocation) {
    for word in &inv.words {
        v.visit_word(word);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Invocation, InvocationContext, Span, Word, WordPiece};

    fn static_word(text: &str) -> Word {
        Word {
            span: Span::new(0, text.len()),
            pieces: vec![WordPiece::Literal {
                text: text.to_string(),
                span: Span::new(0, text.len()),
            }],
            static_value: Some(text.to_string()),
        }
    }

    #[derive(Default)]
    struct Counter {
        units: usize,
        commands: usize,
        invocations: usize,
        words: usize,
    }

    impl Visitor for Counter {
        fn visit_unit(&mut self, unit: &SourceUnit) {
            self.units += 1;
            walk_unit(self, unit);
        }
        fn visit_command(&mut self, cmd: &CommandLike) {
            self.commands += 1;
            walk_command(self, cmd);
        }
        fn visit_invocation(&mut self, inv: &Invocation) {
            self.invocations += 1;
            walk_invocation(self, inv);
        }
        fn visit_word(&mut self, _word: &Word) {
            self.words += 1;
        }
    }

    #[test]
    fn visitor_counts_default_walk() {
        let inv = Invocation {
            words: vec![static_word("git"), static_word("status")],
            span: Span::new(0, 10),
            context: InvocationContext::Default,
        };
        let unit = SourceUnit {
            source_id: crate::ir::SourceId::new(0),
            commands: vec![CommandLike::Simple(inv)],
            functions_defined: vec![],
            aliases_defined: vec![],
            var_assignments: vec![],
        };
        let mut c = Counter::default();
        c.visit_unit(&unit);
        assert_eq!(c.units, 1);
        assert_eq!(c.commands, 1);
        assert_eq!(c.invocations, 1);
        assert_eq!(c.words, 2);
    }
}
