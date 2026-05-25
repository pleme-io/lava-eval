//! Minimal s-expression parser for the `.tlisp` architecture form.
//!
//! Authored to match the surface the lava deflava-architecture form
//! uses: symbols, keywords (`:foo`), strings (with `{var}` interpolation
//! handled by the evaluator, not the parser), booleans (#t/#f), lists.

use thiserror::Error;

#[derive(Debug, Clone, PartialEq)]
pub enum Atom {
    Sym(String),
    Kw(String),
    Str(String),
    Bool(bool),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Sx {
    Atom(Atom),
    List(Vec<Sx>),
}

impl Sx {
    pub fn as_list(&self) -> Option<&[Sx]> {
        if let Sx::List(xs) = self { Some(xs) } else { None }
    }
    pub fn as_sym(&self) -> Option<&str> {
        if let Sx::Atom(Atom::Sym(s)) = self { Some(s) } else { None }
    }
    pub fn as_kw(&self) -> Option<&str> {
        if let Sx::Atom(Atom::Kw(k)) = self { Some(k) } else { None }
    }
    pub fn as_str(&self) -> Option<&str> {
        if let Sx::Atom(Atom::Str(s)) = self { Some(s) } else { None }
    }
    pub fn as_bool(&self) -> Option<bool> {
        if let Sx::Atom(Atom::Bool(b)) = self { Some(*b) } else { None }
    }
}

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("unexpected character {0:?} at offset {1}")]
    UnexpectedChar(char, usize),
    #[error("unterminated string starting at offset {0}")]
    UnterminatedString(usize),
    #[error("unclosed list starting at offset {0}")]
    UnclosedList(usize),
    #[error("unexpected close paren at offset {0}")]
    UnexpectedClose(usize),
    #[error("expected single top-level form, got {0}")]
    MultipleForms(usize),
}

pub fn parse(src: &str) -> Result<Sx, ParseError> {
    let mut p = Parser { src: src.as_bytes(), pos: 0 };
    p.skip();
    let form = p.read_one()?;
    p.skip();
    if p.pos < p.src.len() {
        return Err(ParseError::MultipleForms(p.pos));
    }
    Ok(form)
}

struct Parser<'a> {
    src: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }
    fn bump(&mut self) -> Option<u8> {
        let c = self.peek()?;
        self.pos += 1;
        Some(c)
    }
    fn skip(&mut self) {
        while let Some(c) = self.peek() {
            match c {
                b' ' | b'\t' | b'\n' | b'\r' | b',' => { self.pos += 1; }
                b';' => {
                    while let Some(c) = self.peek() {
                        self.pos += 1;
                        if c == b'\n' { break; }
                    }
                }
                _ => break,
            }
        }
    }
    fn read_one(&mut self) -> Result<Sx, ParseError> {
        self.skip();
        let start = self.pos;
        match self.peek() {
            None => Err(ParseError::UnexpectedClose(start)),
            Some(b'(') => self.read_list(),
            Some(b')') => Err(ParseError::UnexpectedClose(start)),
            Some(b'"') => self.read_string(),
            Some(b'#') => self.read_hash_lit(),
            Some(b':') => self.read_keyword(),
            Some(_) => self.read_symbol(),
        }
    }
    fn read_list(&mut self) -> Result<Sx, ParseError> {
        let start = self.pos;
        self.bump(); // (
        let mut out = Vec::new();
        loop {
            self.skip();
            match self.peek() {
                None => return Err(ParseError::UnclosedList(start)),
                Some(b')') => { self.bump(); break; }
                Some(_) => out.push(self.read_one()?),
            }
        }
        Ok(Sx::List(out))
    }
    fn read_string(&mut self) -> Result<Sx, ParseError> {
        let start = self.pos;
        self.bump(); // "
        let mut out = String::new();
        while let Some(c) = self.peek() {
            match c {
                b'"' => { self.bump(); return Ok(Sx::Atom(Atom::Str(out))); }
                b'\\' => {
                    self.bump();
                    match self.bump() {
                        Some(b'n') => out.push('\n'),
                        Some(b'r') => out.push('\r'),
                        Some(b't') => out.push('\t'),
                        Some(b'\\') => out.push('\\'),
                        Some(b'"') => out.push('"'),
                        Some(other) => out.push(other as char),
                        None => return Err(ParseError::UnterminatedString(start)),
                    }
                }
                _ => { self.bump(); out.push(c as char); }
            }
        }
        Err(ParseError::UnterminatedString(start))
    }
    fn read_hash_lit(&mut self) -> Result<Sx, ParseError> {
        let start = self.pos;
        self.bump(); // #
        match self.bump() {
            Some(b't') => Ok(Sx::Atom(Atom::Bool(true))),
            Some(b'f') => Ok(Sx::Atom(Atom::Bool(false))),
            Some(c) => Err(ParseError::UnexpectedChar(c as char, start)),
            None => Err(ParseError::UnexpectedClose(start)),
        }
    }
    fn read_keyword(&mut self) -> Result<Sx, ParseError> {
        self.bump(); // :
        let body = self.read_atom_body();
        Ok(Sx::Atom(Atom::Kw(body)))
    }
    fn read_symbol(&mut self) -> Result<Sx, ParseError> {
        let body = self.read_atom_body();
        Ok(Sx::Atom(Atom::Sym(body)))
    }
    fn read_atom_body(&mut self) -> String {
        let mut out = String::new();
        while let Some(c) = self.peek() {
            if matches!(c, b' ' | b'\t' | b'\n' | b'\r' | b'(' | b')' | b';' | b',') {
                break;
            }
            self.bump();
            out.push(c as char);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_atoms() {
        assert_eq!(parse("hello").unwrap(), Sx::Atom(Atom::Sym("hello".into())));
        assert_eq!(parse(":name").unwrap(), Sx::Atom(Atom::Kw("name".into())));
        assert_eq!(parse("\"abc\"").unwrap(), Sx::Atom(Atom::Str("abc".into())));
        assert_eq!(parse("#t").unwrap(), Sx::Atom(Atom::Bool(true)));
        assert_eq!(parse("#f").unwrap(), Sx::Atom(Atom::Bool(false)));
    }

    #[test]
    fn parses_nested_list_with_keywords_and_strings() {
        let r = parse("(deflava-architecture aws-vpc-network :inputs ((:cidr \"10.0.0.0/16\")))").unwrap();
        let xs = r.as_list().unwrap();
        assert_eq!(xs[0].as_sym(), Some("deflava-architecture"));
        assert_eq!(xs[1].as_sym(), Some("aws-vpc-network"));
        assert_eq!(xs[2].as_kw(), Some("inputs"));
    }

    #[test]
    fn skips_line_comments_and_commas() {
        // Commas treated as whitespace per Clojure tradition; comments
        // are line-terminated with `;`.
        let r = parse("(a ;; comment\n b , c)").unwrap();
        let xs = r.as_list().unwrap();
        assert_eq!(xs.len(), 3);
        assert_eq!(xs[0].as_sym(), Some("a"));
    }
}
