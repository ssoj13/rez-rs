// SPDX-License-Identifier: Apache-2.0

//! Shared fnmatch pattern translation and compilation.

use regex::{Regex, RegexBuilder};

/// Convert a fnmatch pattern to an anchored regex pattern.
#[doc(hidden)]
pub fn glob_to_regex(glob: &str) -> String {
    let chars: Vec<char> = glob.chars().collect();
    let mut regex = String::with_capacity(glob.len() * 2);
    regex.push_str(r"\A");

    let mut index = 0;
    while index < chars.len() {
        match chars[index] {
            '*' => regex.push_str(".*"),
            '?' => regex.push('.'),
            '[' => {
                let class_start = index + 1;
                let mut close = class_start;
                if close < chars.len() && chars[close] == '!' {
                    close += 1;
                }
                if close < chars.len() && chars[close] == ']' {
                    close += 1;
                }
                while close < chars.len() && chars[close] != ']' {
                    close += 1;
                }
                if close == chars.len() {
                    regex.push_str(r"\[");
                } else {
                    let negated = chars[class_start] == '!';
                    let content_start = class_start + if negated { 1 } else { 0 };
                    let content = &chars[content_start..close];
                    if content.is_empty() {
                        regex.push_str(r"[^\s\S]");
                    } else if negated && content == ['!'] {
                        regex.push('.');
                    } else {
                        let mut chunks: Vec<Vec<char>> = Vec::new();
                        if content.contains(&'-') {
                            let mut start = content_start;
                            let mut search = content_start + 1;
                            while search < close {
                                if chars[search] == '-' {
                                    chunks.push(chars[start..search].to_vec());
                                    start = search + 1;
                                    search += 3;
                                } else {
                                    search += 1;
                                }
                            }
                            if start < close {
                                chunks.push(chars[start..close].to_vec());
                            } else if let Some(last) = chunks.last_mut() {
                                last.push('-');
                            }
                            for chunk in (1..chunks.len()).rev() {
                                let merge = {
                                    let (left, right) = chunks.split_at_mut(chunk);
                                    let left = &mut left[chunk - 1];
                                    let right = &right[0];
                                    if left.last().copied() > right.first().copied() {
                                        left.pop();
                                        left.extend(right.iter().skip(1));
                                        true
                                    } else {
                                        false
                                    }
                                };
                                if merge {
                                    chunks.remove(chunk);
                                }
                            }
                        } else {
                            chunks.push(content.to_vec());
                        }

                        if chunks.is_empty() || (chunks.len() == 1 && chunks[0].is_empty()) {
                            regex.push_str(r"[^\s\S]");
                        } else {
                            regex.push('[');
                            if negated {
                                regex.push('^');
                            } else if matches!(content[0], '^' | '[') {
                                regex.push('\\');
                            }
                            for (chunk_index, chunk) in chunks.iter().enumerate() {
                                if chunk_index > 0 {
                                    regex.push('-');
                                }
                                for character in chunk {
                                    match character {
                                        '\\' | ']' | '&' | '~' | '|' | '-' => {
                                            regex.push('\\');
                                            regex.push(*character);
                                        }
                                        character => regex.push(*character),
                                    }
                                }
                            }
                            regex.push(']');
                        }
                    }
                    index = close;
                }
            }
            character => regex.push_str(&regex::escape(&character.to_string())),
        }
        index += 1;
    }

    regex.push_str(r"\z");
    regex
}

/// Compile a fnmatch pattern with Python fnmatch platform case and newline behavior.
#[doc(hidden)]
pub fn fnmatch_regex(
    glob: &str,
    case_insensitive: bool,
) -> std::result::Result<Regex, regex::Error> {
    RegexBuilder::new(&glob_to_regex(glob))
        .case_insensitive(case_insensitive)
        .dot_matches_new_line(true)
        .build()
}
