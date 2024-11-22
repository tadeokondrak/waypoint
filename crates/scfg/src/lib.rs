//! A parser for [scfg](https://git.sr.ht/~emersion/scfg).

use std::fmt;

#[derive(Debug)]
pub struct Directive {
    pub name: String,
    pub params: Vec<String>,
    pub children: Vec<Directive>,
    pub line: usize,
}

#[derive(Debug)]
pub struct Error {
    pub expected: char,
    pub line: usize,
    pub column: usize,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "line {}, column {}: expected {:?}",
            self.line, self.column, self.expected
        )
    }
}

impl std::error::Error for Error {}

#[derive(Debug)]
struct Parser<'a> {
    text: &'a str,
    pos: usize,
    line: usize,
    column: usize,
}

impl<'a> Parser<'a> {
    fn new(text: &'a str) -> Parser<'a> {
        Parser {
            text,
            pos: 0,
            line: 0,
            column: 0,
        }
    }

    fn skip_wsp(&mut self) {
        while self.text[self.pos..].starts_with([' ', '\t']) {
            self.pos += 1;
            self.column += 1;
        }
    }

    fn skip_newline(&mut self) {
        loop {
            self.skip_wsp();
            if self.text[self.pos..].starts_with('\n') {
                self.pos += 1;
                self.line += 1;
                self.column = 0;
                continue;
            }
            if self.text[self.pos..].starts_with('#') {
                let len = self.text[self.pos..]
                    .find('\n')
                    .unwrap_or(self.text.len() - self.pos);
                self.pos += len;
                self.line += 1;
                self.column = 0;
                continue;
            }
            break;
        }
        self.skip_wsp();
    }

    fn at(&self, expected: char) -> bool {
        self.text[self.pos..].starts_with(expected)
    }

    fn at_end(&self) -> bool {
        self.pos == self.text.len()
    }

    fn expect(&mut self, expected: char) -> Result<(), Error> {
        if !self.text[self.pos..].starts_with(expected) {
            Err(Error {
                expected,
                line: self.line,
                column: self.column,
            })
        } else {
            self.pos += expected.len_utf8();
            self.column += expected.len_utf8();
            Ok(())
        }
    }
}

pub fn parse(text: &str) -> Result<Vec<Directive>, Error> {
    let mut p = Parser::new(text);
    parse_config(&mut p)
}

fn parse_config(p: &mut Parser) -> Result<Vec<Directive>, Error> {
    let mut directives = Vec::new();
    p.skip_newline();
    while !p.at_end() {
        directives.push(parse_directive(p)?);
    }
    Ok(directives)
}

fn parse_directive(p: &mut Parser) -> Result<Directive, Error> {
    let line = p.line;
    let name = parse_word(p)?;
    p.skip_wsp();
    let params = parse_directive_params(p)?;
    p.skip_wsp();
    let directives = if p.at('{') {
        parse_block(p)?
    } else {
        Vec::default()
    };
    p.skip_newline();
    Ok(Directive {
        name,
        params,
        children: directives,
        line,
    })
}

fn parse_directive_params(p: &mut Parser) -> Result<Vec<String>, Error> {
    let mut params = Vec::new();
    while !p.at('\n') && !p.at('{') && !p.at_end() {
        params.push(parse_word(p)?);
        p.skip_wsp();
    }
    Ok(params)
}

fn parse_block(p: &mut Parser) -> Result<Vec<Directive>, Error> {
    let mut directives = Vec::new();
    p.expect('{')?;
    p.skip_newline();
    while !p.at('}') && !p.at_end() {
        directives.push(parse_directive(p)?);
    }
    p.expect('}')?;
    Ok(directives)
}

fn parse_word(p: &mut Parser) -> Result<String, Error> {
    if p.at('"') {
        parse_dquote_word(p)
    } else if p.at('\'') {
        parse_squote_word(p)
    } else {
        parse_atom(p)
    }
}

fn parse_atom(p: &mut Parser<'_>) -> Result<String, Error> {
    let word = parse_word_impl(p, true, |c| {
        matches!(
            c,
            '\u{21}'
            | '\u{23}'..='\u{26}'
            | '\u{28}'..='\u{5B}'
            | '\u{5D}'..='\u{7A}'
            | '\u{7C}'
            | '\u{7E}'
            | '\u{80}'..='\u{10FFFF}',
        )
    })?;
    Ok(word)
}

fn parse_dquote_word(p: &mut Parser<'_>) -> Result<String, Error> {
    p.expect('"')?;
    let word = parse_word_impl(p, true, |c| {
        matches!(
            c,
            '\u{21}'
            | '\u{23}'..='\u{26}'
            | '\u{28}'..='\u{5B}'
            | '\u{5D}'..='\u{7A}'
            | '\u{7C}'
            | '\u{7E}'
            | '\u{80}'..='\u{10FFFF}'
            | '\''
            | '{'
            | '}'
            | ' '
            | '\t',

        )
    });
    p.expect('"')?;
    word
}

fn parse_squote_word(p: &mut Parser) -> Result<String, Error> {
    p.expect('\'')?;
    let word = parse_word_impl(p, false, |c| {
        matches!(
            c,
            '\u{09}'
            | '\u{20}'..='\u{26}'
            | '\u{28}'..='\u{7E}'
            | '\u{80}'..='\u{10FFFF}',
        )
    });
    p.expect('\'')?;
    word
}

fn parse_word_impl(
    p: &mut Parser<'_>,
    allow_escaped: bool,
    ok: impl Fn(char) -> bool,
) -> Result<String, Error> {
    let mut chars = p.text[p.pos..].chars();
    let mut atom = String::new();
    let mut escaped = false;
    loop {
        match chars.next() {
            Some(c) if ok(c) || (escaped && !c.is_ascii_control() && c != '\n') => {
                p.pos += c.len_utf8();
                p.column += c.len_utf8();
                atom.push(c);
                escaped = false;
            }
            Some('\\') if allow_escaped => {
                p.pos += 1;
                p.column += 1;
                escaped = true;
            }
            Some(_) | None => {
                break Ok(atom);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use expect_test::{expect, Expect};

    #[test]
    fn test_parse() {
        fn check(s: &str, expected: Expect) {
            let result = parse(s);
            expected.assert_debug_eq(&result);
        }

        check(
            "",
            expect![[r#"
                Ok(
                    [],
                )
            "#]],
        );

        check(
            "simple",
            expect![[r#"
                Ok(
                    [
                        Directive {
                            name: "simple",
                            params: [],
                            children: [],
                            line: 0,
                        },
                    ],
                )
            "#]],
        );

        check(
            "unclosed {",
            expect![[r#"
                Err(
                    Error {
                        expected: '}',
                        line: 0,
                        column: 10,
                    },
                )
            "#]],
        );

        check(
            "
                # comment
                directive
                # comment
            ",
            expect![[r#"
                Ok(
                    [
                        Directive {
                            name: "directive",
                            params: [],
                            children: [],
                            line: 3,
                        },
                    ],
                )
            "#]],
        );

        check(
            r#"escaped \' \""#,
            expect![[r#"
                Ok(
                    [
                        Directive {
                            name: "escaped",
                            params: [
                                "'",
                                "\"",
                            ],
                            children: [],
                            line: 0,
                        },
                    ],
                )
            "#]],
        );

        check(
            r#"train "Shinkansen" {
                model "E5" {
                    max-speed 320km/h
                    weight 453.5t

                    lines-served "Tōhoku" "Hokkaido"
                }

                model "E7" {
                    max-speed 275km/h
                    weight 540t

                    lines-served "Hokuriku" "Jōetsu"
                }
            }"#,
            expect![[r#"
                Ok(
                    [
                        Directive {
                            name: "train",
                            params: [
                                "Shinkansen",
                            ],
                            children: [
                                Directive {
                                    name: "model",
                                    params: [
                                        "E5",
                                    ],
                                    children: [
                                        Directive {
                                            name: "max-speed",
                                            params: [
                                                "320km/h",
                                            ],
                                            children: [],
                                            line: 2,
                                        },
                                        Directive {
                                            name: "weight",
                                            params: [
                                                "453.5t",
                                            ],
                                            children: [],
                                            line: 3,
                                        },
                                        Directive {
                                            name: "lines-served",
                                            params: [
                                                "Tōhoku",
                                                "Hokkaido",
                                            ],
                                            children: [],
                                            line: 5,
                                        },
                                    ],
                                    line: 1,
                                },
                                Directive {
                                    name: "model",
                                    params: [
                                        "E7",
                                    ],
                                    children: [
                                        Directive {
                                            name: "max-speed",
                                            params: [
                                                "275km/h",
                                            ],
                                            children: [],
                                            line: 9,
                                        },
                                        Directive {
                                            name: "weight",
                                            params: [
                                                "540t",
                                            ],
                                            children: [],
                                            line: 10,
                                        },
                                        Directive {
                                            name: "lines-served",
                                            params: [
                                                "Hokuriku",
                                                "Jōetsu",
                                            ],
                                            children: [],
                                            line: 12,
                                        },
                                    ],
                                    line: 8,
                                },
                            ],
                            line: 0,
                        },
                    ],
                )
            "#]],
        );
    }
}
