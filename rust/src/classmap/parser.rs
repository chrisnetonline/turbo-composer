/// PHP reserved keywords that cannot be class/interface/trait/enum names.
const PHP_KEYWORDS: &[&str] = &[
    "abstract",
    "and",
    "array",
    "as",
    "break",
    "callable",
    "case",
    "catch",
    "class",
    "clone",
    "const",
    "continue",
    "declare",
    "default",
    "do",
    "echo",
    "else",
    "elseif",
    "empty",
    "enddeclare",
    "endfor",
    "endforeach",
    "endif",
    "endswitch",
    "endwhile",
    "enum",
    "eval",
    "exit",
    "extends",
    "false",
    "final",
    "finally",
    "fn",
    "for",
    "foreach",
    "function",
    "global",
    "goto",
    "if",
    "implements",
    "include",
    "include_once",
    "instanceof",
    "insteadof",
    "interface",
    "isset",
    "list",
    "match",
    "namespace",
    "new",
    "null",
    "or",
    "print",
    "private",
    "protected",
    "public",
    "readonly",
    "require",
    "require_once",
    "return",
    "self",
    "static",
    "switch",
    "throw",
    "trait",
    "true",
    "try",
    "unset",
    "use",
    "var",
    "while",
    "xor",
    "yield",
];

pub(crate) fn extract_php_symbols(contents: &str) -> Vec<String> {
    let bytes = contents.as_bytes();
    let len = bytes.len();

    let mut symbols = Vec::new();
    let mut namespace: Option<String> = None;
    let mut ns_brace_depth: Option<usize> = None; // For brace-style namespaces
    let mut brace_depth: usize = 0;
    let mut pos: usize = 0;
    let mut prev_was_new = false;
    let mut after_double_colon = false; // Tracks :: to detect SomeClass::class

    while pos < len {
        let b = bytes[pos];

        match b {
            b' ' | b'\t' | b'\r' => {
                pos += 1;
            }
            b'\n' => {
                pos += 1;
            }
            b'/' if pos + 1 < len && bytes[pos + 1] == b'/' => {
                pos += 2;
                while pos < len && bytes[pos] != b'\n' {
                    pos += 1;
                }
            }
            b'/' if pos + 1 < len && bytes[pos + 1] == b'*' => {
                pos += 2;
                while pos + 1 < len {
                    if bytes[pos] == b'*' && bytes[pos + 1] == b'/' {
                        pos += 2;
                        break;
                    }
                    pos += 1;
                }
                if pos + 1 >= len {
                    pos = len;
                }
            }
            b'#' if pos + 1 < len && bytes[pos + 1] != b'[' => {
                pos += 1;
                while pos < len && bytes[pos] != b'\n' {
                    pos += 1;
                }
            }
            b'#' if pos + 1 < len && bytes[pos + 1] == b'[' => {
                pos += 2;
                let mut depth = 1u32;
                while pos < len && depth > 0 {
                    match bytes[pos] {
                        b'[' => depth += 1,
                        b']' => depth -= 1,
                        _ => {}
                    }
                    pos += 1;
                }
            }
            b'\'' => {
                pos += 1;
                while pos < len {
                    if bytes[pos] == b'\\' && pos + 1 < len {
                        pos += 2;
                        continue;
                    }
                    if bytes[pos] == b'\'' {
                        pos += 1;
                        break;
                    }
                    pos += 1;
                }
            }
            b'"' => {
                pos += 1;
                while pos < len {
                    if bytes[pos] == b'\\' && pos + 1 < len {
                        pos += 2;
                        continue;
                    }
                    if bytes[pos] == b'"' {
                        pos += 1;
                        break;
                    }
                    pos += 1;
                }
            }
            b'<' if pos + 2 < len && bytes[pos + 1] == b'<' && bytes[pos + 2] == b'<' => {
                pos += 3;
                while pos < len && (bytes[pos] == b' ' || bytes[pos] == b'\'' || bytes[pos] == b'"')
                {
                    pos += 1;
                }
                let label_start = pos;
                while pos < len && (bytes[pos].is_ascii_alphanumeric() || bytes[pos] == b'_') {
                    pos += 1;
                }
                let label = &bytes[label_start..pos];
                if label.is_empty() {
                    continue;
                }
                while pos < len && bytes[pos] != b'\n' {
                    pos += 1;
                }
                if pos < len {
                    pos += 1;
                }
                while pos < len {
                    if bytes[pos] == b'\n' || pos == 0 || (pos > 0 && bytes[pos - 1] == b'\n') {
                        let line_start = pos;
                        while pos < len && (bytes[pos] == b' ' || bytes[pos] == b'\t') {
                            pos += 1;
                        }
                        if pos + label.len() <= len && &bytes[pos..pos + label.len()] == label {
                            pos += label.len();
                            if pos >= len || bytes[pos] == b';' || bytes[pos] == b'\n' {
                                while pos < len && bytes[pos] != b'\n' {
                                    pos += 1;
                                }
                                break;
                            }
                        }
                        if pos == line_start {
                            pos += 1;
                        }
                    } else {
                        pos += 1;
                    }
                }
            }
            b'{' => {
                brace_depth += 1;
                pos += 1;
                prev_was_new = false;
            }
            b'}' => {
                brace_depth = brace_depth.saturating_sub(1);
                if let Some(ns_depth) = ns_brace_depth {
                    if brace_depth == ns_depth {
                        namespace = None;
                        ns_brace_depth = None;
                    }
                }
                pos += 1;
                prev_was_new = false;
            }
            b'a'..=b'z' | b'A'..=b'Z' | b'_' => {
                let word_start = pos;
                while pos < len && (bytes[pos].is_ascii_alphanumeric() || bytes[pos] == b'_') {
                    pos += 1;
                }
                let word = &bytes[word_start..pos];

                match word {
                    b"namespace" => {
                        let ns = read_namespace_name(bytes, &mut pos);
                        if !ns.is_empty() {
                            skip_whitespace(bytes, &mut pos);
                            if pos < len && bytes[pos] == b'{' {
                                ns_brace_depth = Some(brace_depth);
                                brace_depth += 1;
                                pos += 1;
                            }
                            namespace = Some(ns);
                        }
                        prev_was_new = false;
                        after_double_colon = false;
                    }
                    b"class" | b"interface" | b"trait" | b"enum" => {
                        if !prev_was_new && !after_double_colon {
                            skip_whitespace(bytes, &mut pos);
                            let name = read_identifier(bytes, &mut pos);
                            if !name.is_empty() && !PHP_KEYWORDS.contains(&name.as_str()) {
                                let fqcn = match &namespace {
                                    Some(ns) => format!("{ns}\\{name}"),
                                    None => name,
                                };
                                symbols.push(fqcn);
                            }
                        }
                        prev_was_new = false;
                        after_double_colon = false;
                    }
                    b"new" => {
                        prev_was_new = true;
                        after_double_colon = false;
                    }
                    // These precede class — don't reset prev_was_new
                    b"abstract" | b"final" | b"readonly" => {
                        after_double_colon = false;
                    }

                    _ => {
                        prev_was_new = false;
                        after_double_colon = false;
                    }
                }
            }
            _ => {
                if b == b':' && pos + 1 < len && bytes[pos + 1] == b':' {
                    after_double_colon = true;
                    pos += 2;
                    prev_was_new = false;
                } else {
                    pos += 1;
                    if b != b' ' && b != b'\t' && b != b'\n' && b != b'\r' {
                        prev_was_new = false;
                        after_double_colon = false;
                    }
                }
            }
        }
    }

    symbols
}

#[inline]
pub(crate) fn contains_class_keyword(bytes: &[u8]) -> bool {
    use aho_corasick::AhoCorasick;
    use std::sync::LazyLock;

    static AC: LazyLock<AhoCorasick> =
        LazyLock::new(|| AhoCorasick::new(["class", "interface", "trait", "enum"]).unwrap());

    AC.is_match(bytes)
}

#[inline]
fn skip_whitespace(bytes: &[u8], pos: &mut usize) {
    while *pos < bytes.len() && matches!(bytes[*pos], b' ' | b'\t' | b'\n' | b'\r') {
        *pos += 1;
    }
}

#[inline]
fn read_identifier(bytes: &[u8], pos: &mut usize) -> String {
    let start = *pos;
    while *pos < bytes.len() && (bytes[*pos].is_ascii_alphanumeric() || bytes[*pos] == b'_') {
        *pos += 1;
    }
    // Safety: bytes are guaranteed ASCII alphanumeric + underscore
    unsafe { String::from_utf8_unchecked(bytes[start..*pos].to_vec()) }
}

fn read_namespace_name(bytes: &[u8], pos: &mut usize) -> String {
    skip_whitespace(bytes, pos);
    let start = *pos;
    while *pos < bytes.len()
        && (bytes[*pos].is_ascii_alphanumeric() || bytes[*pos] == b'_' || bytes[*pos] == b'\\')
    {
        *pos += 1;
    }
    // Safety: bytes are guaranteed ASCII alphanumeric + underscore + backslash
    unsafe { String::from_utf8_unchecked(bytes[start..*pos].to_vec()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_simple_class() {
        let symbols = extract_php_symbols("<?php\nclass Foo {}\n");
        assert_eq!(symbols, vec!["Foo"]);
    }

    #[test]
    fn extract_namespaced_class() {
        let symbols = extract_php_symbols("<?php\nnamespace App\\Models;\n\nclass User {}\n");
        assert_eq!(symbols, vec!["App\\Models\\User"]);
    }

    #[test]
    fn extract_interface() {
        let symbols =
            extract_php_symbols("<?php\nnamespace App\\Contracts;\n\ninterface Cacheable {}\n");
        assert_eq!(symbols, vec!["App\\Contracts\\Cacheable"]);
    }

    #[test]
    fn extract_trait() {
        let symbols =
            extract_php_symbols("<?php\nnamespace App\\Concerns;\n\ntrait HasTimestamps {}\n");
        assert_eq!(symbols, vec!["App\\Concerns\\HasTimestamps"]);
    }

    #[test]
    fn extract_enum() {
        let symbols = extract_php_symbols(
            "<?php\nnamespace App\\Enums;\n\nenum Status: string {\n    case Active = 'active';\n}\n",
        );
        assert_eq!(symbols, vec!["App\\Enums\\Status"]);
    }

    #[test]
    fn extract_multiple_classes_same_namespace() {
        let symbols =
            extract_php_symbols("<?php\nnamespace App\\Models;\n\nclass User {}\nclass Post {}\n");
        assert_eq!(symbols, vec!["App\\Models\\User", "App\\Models\\Post"]);
    }

    #[test]
    fn extract_multiple_namespaces_brace_style() {
        let symbols = extract_php_symbols(
            "<?php\nnamespace Foo {\n    class Bar {}\n}\nnamespace Baz {\n    class Qux {}\n}\n",
        );
        assert_eq!(symbols, vec!["Foo\\Bar", "Baz\\Qux"]);
    }

    #[test]
    fn extract_abstract_and_final_classes() {
        let symbols = extract_php_symbols(
            "<?php\nnamespace App;\n\nabstract class BaseModel {}\nfinal class ConcreteModel {}\n",
        );
        assert_eq!(symbols, vec!["App\\BaseModel", "App\\ConcreteModel"]);
    }

    #[test]
    fn extract_no_symbols_from_function_file() {
        let symbols = extract_php_symbols("<?php\nfunction helper_func() { return 42; }\n");
        assert!(symbols.is_empty());
    }

    #[test]
    fn extract_no_symbols_from_config_file() {
        let symbols = extract_php_symbols("<?php\nreturn ['key' => 'value'];\n");
        assert!(symbols.is_empty());
    }

    #[test]
    fn extract_class_without_namespace() {
        let symbols = extract_php_symbols("<?php\nclass GlobalClass {}\n");
        assert_eq!(symbols, vec!["GlobalClass"]);
    }

    #[test]
    fn extract_ignores_class_in_comment() {
        let symbols = extract_php_symbols(
            "<?php\nnamespace App;\n\n// class FakeClass {}\n/* class AnotherFake {} */\nclass RealClass {}\n",
        );
        assert_eq!(symbols, vec!["App\\RealClass"]);
    }

    #[test]
    fn extract_ignores_class_in_string() {
        let symbols = extract_php_symbols(
            "<?php\nnamespace App;\n\n$x = \"class NotAClass {}\";\nclass ActualClass {}\n",
        );
        assert_eq!(symbols, vec!["App\\ActualClass"]);
    }

    #[test]
    fn extract_class_with_extends_and_implements() {
        let symbols = extract_php_symbols(
            "<?php\nnamespace App;\n\nclass UserController extends Controller implements HasMiddleware {}\n",
        );
        assert_eq!(symbols, vec!["App\\UserController"]);
    }

    #[test]
    fn extract_readonly_class() {
        let symbols =
            extract_php_symbols("<?php\nnamespace App\\DTO;\n\nreadonly class UserData {}\n");
        assert_eq!(symbols, vec!["App\\DTO\\UserData"]);
    }

    #[test]
    fn ignores_double_colon_class_constant() {
        let symbols =
            extract_php_symbols("<?php\nnamespace App;\nclass Foo {}\n$x = Foo::class;\n");
        assert_eq!(symbols, vec!["App\\Foo"]);
    }

    #[test]
    fn ignores_double_colon_class_instanceof() {
        // SomeClass::class instanceof SomeInterface — should not extract "instanceof"
        let symbols = extract_php_symbols(
            "<?php\nnamespace App;\nclass Foo {}\nif (Foo::class instanceof Bar) {}\n",
        );
        assert_eq!(symbols, vec!["App\\Foo"]);
    }

    #[test]
    fn ignores_keyword_as_class_name() {
        // Reserved keywords should never be extracted as class names
        let symbols =
            extract_php_symbols("<?php\nnamespace App;\nclass Foo {}\n$x = SomeClass::class;\n");
        assert_eq!(symbols, vec!["App\\Foo"]);
        // The ::class constant should not generate any extra entry
    }

    #[test]
    fn ignores_class_in_match_expression() {
        let symbols = extract_php_symbols(
            "<?php\nnamespace App;\nclass Foo {}\nmatch (true) {\n    Foo::class => 'foo',\n}\n",
        );
        assert_eq!(symbols, vec!["App\\Foo"]);
    }
}
