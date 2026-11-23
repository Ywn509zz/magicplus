//! Coloration des blocs de code (```rust, ```python…), sans dépendance : un petit analyseur
//! lexical commun, réglé pour chaque famille de langages. Couleurs du thème « One Dark »,
//! affichées sur un fond sombre dans les deux thèmes de l'application.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Plain,
    Keyword,
    String,
    Comment,
    Number,
    /// Type, constante, clé (JSON, YAML…) ou balise (HTML).
    Type,
    Function,
}

impl TokenKind {
    /// Couleur 0xRRGGBB (fond de code sombre).
    pub fn color(self) -> u32 {
        match self {
            TokenKind::Plain => 0xABB2BF,
            TokenKind::Keyword => 0xC678DD,
            TokenKind::String => 0x98C379,
            TokenKind::Comment => 0x7F848E,
            TokenKind::Number => 0xD19A66,
            TokenKind::Type => 0xE5C07B,
            TokenKind::Function => 0x61AFEF,
        }
    }
}

/// Fond des blocs de code.
pub const CODE_BACKGROUND: u32 = 0x282C34;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub text: String,
    pub kind: TokenKind,
}

struct Syntax {
    line_comments: &'static [&'static str],
    block_comment: Option<(&'static str, &'static str)>,
    quotes: &'static [char],
    triple_quotes: bool,
    keywords: &'static [&'static str],
    case_insensitive: bool,
    /// Un mot ou une chaîne suivis de `:` ou `=` est une clé (JSON, YAML, TOML).
    keys: bool,
    /// `<balise` et `</balise>` (HTML, XML).
    tags: bool,
    /// Un identifiant qui commence par une majuscule est un type.
    capital_types: bool,
}

const RUST: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "false", "fn", "for", "if", "impl",
    "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super", "trait",
    "true", "type", "unsafe", "use", "where", "while", "Some", "None", "Ok", "Err",
];
const JS: &[&str] = &[
    "async", "await", "break", "case", "catch", "class", "const", "continue", "default", "delete", "do", "else", "export", "extends",
    "false", "finally", "for", "from", "function", "if", "import", "in", "instanceof", "interface", "let", "new", "null", "of", "return",
    "static", "super", "switch", "this", "throw", "true", "try", "type", "typeof", "undefined", "var", "void", "while", "yield",
];
const C_LIKE: &[&str] = &[
    "auto", "bool", "break", "case", "catch", "char", "class", "const", "continue", "default", "delete", "do", "double", "else", "enum",
    "extends", "false", "final", "float", "for", "func", "go", "if", "implements", "import", "include", "int", "interface", "long", "nil",
    "namespace", "new", "null", "package", "private", "protected", "public", "return", "short", "signed", "static", "struct", "switch",
    "this", "throw", "true", "try", "typedef", "unsigned", "using", "var", "void", "while", "val", "fun", "let", "guard", "defer", "chan",
    "select", "range", "map", "string", "override", "virtual", "template", "typename", "self", "init",
];
const PYTHON: &[&str] = &[
    "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif", "else", "except", "False", "finally",
    "for", "from", "global", "if", "import", "in", "is", "lambda", "None", "nonlocal", "not", "or", "pass", "raise", "return", "True",
    "try", "while", "with", "yield", "self", "print",
];
const SHELL: &[&str] = &[
    "if", "then", "else", "elif", "fi", "for", "in", "do", "done", "while", "until", "case", "esac", "function", "return", "exit", "local",
    "export", "echo", "cd", "sudo", "set", "unset", "source", "alias", "true", "false",
];
const SQL: &[&str] = &[
    "select", "from", "where", "and", "or", "not", "insert", "into", "values", "update", "set", "delete", "create", "table", "drop",
    "alter", "add", "join", "left", "right", "inner", "outer", "on", "as", "group", "by", "order", "having", "limit", "offset", "null",
    "is", "primary", "key", "foreign", "references", "index", "distinct", "count", "sum", "avg", "min", "max", "union", "all", "case",
    "when", "then", "else", "end", "asc", "desc", "integer", "text", "varchar", "default",
];
const LUA: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "if", "in", "local", "nil", "not", "or", "repeat",
    "return", "then", "true", "until", "while",
];
const DATA: &[&str] = &["true", "false", "null", "yes", "no", "on", "off"];
const CSS: &[&str] = &["important", "media", "import", "from", "to", "inherit", "initial", "none", "auto"];

fn syntax_for(language: &str) -> Option<Syntax> {
    let base = Syntax {
        line_comments: &["//"],
        block_comment: Some(("/*", "*/")),
        quotes: &['"', '\''],
        triple_quotes: false,
        keywords: &[],
        case_insensitive: false,
        keys: false,
        tags: false,
        capital_types: true,
    };
    let syntax = match language.to_ascii_lowercase().as_str() {
        "rust" | "rs" => Syntax { keywords: RUST, ..base },
        "js" | "javascript" | "jsx" | "ts" | "typescript" | "tsx" | "mjs" => Syntax { keywords: JS, quotes: &['"', '\'', '`'], ..base },
        "c" | "h" | "cpp" | "c++" | "hpp" | "cc" | "cs" | "csharp" | "java" | "kt" | "kotlin" | "swift" | "go" | "dart" | "scala" | "php"
        | "objc" | "m" => Syntax { keywords: C_LIKE, quotes: &['"', '\'', '`'], ..base },
        "py" | "python" => Syntax {
            line_comments: &["#"],
            block_comment: None,
            triple_quotes: true,
            keywords: PYTHON,
            ..base
        },
        "sh" | "bash" | "zsh" | "shell" | "console" | "ps1" | "powershell" | "bat" | "cmd" => Syntax {
            line_comments: &["#", "REM "],
            block_comment: None,
            keywords: SHELL,
            capital_types: false,
            ..base
        },
        "sql" => Syntax { line_comments: &["--"], quotes: &['\''], keywords: SQL, case_insensitive: true, capital_types: false, ..base },
        "lua" => Syntax { line_comments: &["--"], block_comment: Some(("--[[", "]]")), keywords: LUA, ..base },
        "json" | "jsonc" => Syntax { quotes: &['"'], keywords: DATA, keys: true, capital_types: false, ..base },
        "yaml" | "yml" | "toml" | "ini" | "conf" | "env" => Syntax {
            line_comments: &["#", ";"],
            block_comment: None,
            keywords: DATA,
            keys: true,
            capital_types: false,
            ..base
        },
        "html" | "xml" | "svg" | "vue" => Syntax {
            line_comments: &[],
            block_comment: Some(("<!--", "-->")),
            quotes: &['"', '\''],
            tags: true,
            capital_types: false,
            ..base
        },
        "css" | "scss" | "less" => Syntax { line_comments: &["//"], keywords: CSS, capital_types: false, keys: true, ..base },
        _ => return None,
    };
    Some(syntax)
}

/// Colore `code` (déjà sans tabulations) ligne par ligne. Langage inconnu ou absent : texte brut.
pub fn highlight(language: &str, code: &str) -> Vec<Vec<Token>> {
    let mut lines: Vec<Vec<Token>> = vec![Vec::new()];
    let mut push = |text: &str, kind: TokenKind| {
        for (index, piece) in text.split('\n').enumerate() {
            if index > 0 {
                lines.push(Vec::new());
            }
            if piece.is_empty() {
                continue;
            }
            let line = lines.last_mut().expect("une ligne");
            match line.last_mut() {
                Some(last) if last.kind == kind => last.text.push_str(piece),
                _ => line.push(Token { text: piece.to_string(), kind }),
            }
        }
    };
    let Some(syntax) = syntax_for(language) else {
        push(code, TokenKind::Plain);
        return lines;
    };

    let chars: Vec<char> = code.chars().collect();
    let starts = |i: usize, pattern: &str| pattern.chars().enumerate().all(|(k, p)| chars.get(i + k) == Some(&p));
    let collect = |from: usize, to: usize| chars[from..to].iter().collect::<String>();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        // Commentaire de bloc.
        if let Some((open, close)) = syntax.block_comment {
            if starts(i, open) {
                let mut j = i + open.chars().count();
                while j < chars.len() && !starts(j, close) {
                    j += 1;
                }
                j = (j + close.chars().count()).min(chars.len());
                push(&collect(i, j), TokenKind::Comment);
                i = j;
                continue;
            }
        }
        // Commentaire de ligne.
        if let Some(marker) = syntax.line_comments.iter().find(|m| starts(i, m)) {
            let preceded_ok = marker.len() > 1 || i == 0 || chars[i - 1].is_whitespace();
            if preceded_ok {
                let j = (i..chars.len()).find(|&j| chars[j] == '\n').unwrap_or(chars.len());
                push(&collect(i, j), TokenKind::Comment);
                i = j;
                continue;
            }
        }
        // Chaînes (et chaînes triples en Python).
        if syntax.quotes.contains(&c) {
            let triple = syntax.triple_quotes && starts(i + 1, &format!("{c}{c}"));
            let mut j = i + if triple { 3 } else { 1 };
            while j < chars.len() {
                if chars[j] == '\\' {
                    j += 2;
                    continue;
                }
                if triple {
                    if starts(j, &format!("{c}{c}{c}")) {
                        j += 3;
                        break;
                    }
                } else if chars[j] == c {
                    j += 1;
                    break;
                } else if chars[j] == '\n' && c != '`' {
                    break;
                }
                j += 1;
            }
            let j = j.min(chars.len());
            let next = chars[j..].iter().find(|ch| **ch != ' ');
            let kind = if syntax.keys && matches!(next, Some(':')) { TokenKind::Type } else { TokenKind::String };
            push(&collect(i, j), kind);
            i = j;
            continue;
        }
        // Balises HTML.
        if syntax.tags && c == '<' && chars.get(i + 1).is_some_and(|n| n.is_alphabetic() || *n == '/' || *n == '!') {
            let mut j = i + 1;
            while j < chars.len() && (chars[j].is_alphanumeric() || matches!(chars[j], '/' | '-' | '!' | ':')) {
                j += 1;
            }
            push(&collect(i, j), TokenKind::Keyword);
            i = j;
            continue;
        }
        // Nombres.
        if c.is_ascii_digit() && (i == 0 || !(chars[i - 1].is_alphanumeric() || chars[i - 1] == '_')) {
            let mut j = i;
            while j < chars.len() && (chars[j].is_ascii_alphanumeric() || chars[j] == '_' || chars[j] == '.') {
                j += 1;
            }
            push(&collect(i, j), TokenKind::Number);
            i = j;
            continue;
        }
        // Identifiants et mots-clés.
        if c.is_alphabetic() || c == '_' || (c == '$' && syntax.keywords == SHELL) {
            let mut j = i + 1;
            while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_' || (syntax.keys && chars[j] == '-')) {
                j += 1;
            }
            let word = collect(i, j);
            let keyword = if syntax.case_insensitive {
                syntax.keywords.iter().any(|k| k.eq_ignore_ascii_case(&word))
            } else {
                syntax.keywords.contains(&word.as_str())
            };
            let next = chars[j..].iter().find(|ch| **ch != ' ');
            let kind = if keyword {
                TokenKind::Keyword
            } else if syntax.keys && matches!(next, Some(':') | Some('=')) && (i == 0 || chars[..i].iter().rev().take_while(|c| **c != '\n').all(|c| c.is_whitespace() || *c == '-')) {
                TokenKind::Type
            } else if matches!(next, Some('(')) {
                TokenKind::Function
            } else if syntax.capital_types && word.starts_with(char::is_uppercase) {
                TokenKind::Type
            } else {
                TokenKind::Plain
            };
            push(&word, kind);
            i = j;
            continue;
        }
        push(&c.to_string(), TokenKind::Plain);
        i += 1;
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(language: &str, code: &str) -> Vec<(String, TokenKind)> {
        highlight(language, code).into_iter().flatten().filter(|t| !t.text.trim().is_empty()).map(|t| (t.text.trim().to_string(), t.kind)).collect()
    }

    #[test]
    fn rust_tokens() {
        let tokens = kinds("rust", "fn main() { let x = \"bonjour\"; // salut\n    Vec::new(42) }");
        assert!(tokens.contains(&("fn".into(), TokenKind::Keyword)));
        assert!(tokens.contains(&("main".into(), TokenKind::Function)));
        assert!(tokens.contains(&("\"bonjour\"".into(), TokenKind::String)));
        assert!(tokens.contains(&("// salut".into(), TokenKind::Comment)));
        assert!(tokens.contains(&("Vec".into(), TokenKind::Type)));
        assert!(tokens.contains(&("42".into(), TokenKind::Number)));
    }

    #[test]
    fn python_json_and_unknown() {
        let tokens = kinds("python", "def f():\n    \"\"\"doc\nsur deux lignes\"\"\"\n    return None # fin");
        assert!(tokens.contains(&("def".into(), TokenKind::Keyword)));
        assert!(tokens.contains(&("# fin".into(), TokenKind::Comment)));
        assert_eq!(highlight("python", "\"\"\"a\nb\"\"\"").len(), 2);
        let json = kinds("json", "{\"nom\": \"Léa\", \"ok\": true}");
        assert!(json.contains(&("\"nom\"".into(), TokenKind::Type)));
        assert!(json.contains(&("\"Léa\"".into(), TokenKind::String)));
        assert!(json.contains(&("true".into(), TokenKind::Keyword)));
        assert_eq!(highlight("", "a\nb")[1][0].kind, TokenKind::Plain);
    }
}
