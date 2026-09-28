use std::ops::Range;

pub fn fill(template: &str, values: &[(&str, &str)]) -> String {
    let mut filled = String::with_capacity(template.len());
    let mut copied = 0;
    for (range, value) in placeholders(template, values) {
        filled.push_str(&template[copied..range.start]);
        filled.push_str(value);
        copied = range.end;
    }
    filled.push_str(&template[copied..]);
    filled
}

pub fn frame<'t>(template: &'t str, values: &[(&str, &str)]) -> (&'t str, &'t str, &'t str) {
    let found = placeholders(template, values);
    match (found.first(), found.last()) {
        (Some((first, _)), Some((last, _))) => (
            &template[..first.start],
            &template[first.start..last.end],
            &template[last.end..],
        ),
        _ => (template, "", ""),
    }
}

fn placeholders<'v>(template: &str, values: &[(&str, &'v str)]) -> Vec<(Range<usize>, &'v str)> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(offset) = template[from..].find('{') {
        let start = from + offset;
        let after = &template[start + 1..];
        let named = values.iter().find(|(name, _)| {
            after
                .strip_prefix(name)
                .is_some_and(|rest| rest.starts_with('}'))
        });
        match named {
            Some((name, value)) => {
                let end = start + name.len() + 2;
                found.push((start..end, *value));
                from = end;
            }
            None => from = start + 1,
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALUES: [(&str, &str); 2] = [("session", "work"), ("server", "desktop")];

    #[test]
    fn placeholders_are_replaced_by_their_values() {
        assert_eq!(fill("[{session}@{server}]", &VALUES), "[work@desktop]");
        assert_eq!(fill("{server}/{server}", &VALUES), "desktop/desktop");
        assert_eq!(fill("no fields", &VALUES), "no fields");
        assert_eq!(fill("", &VALUES), "");
        assert_eq!(fill("{session}", &[("session", "{server}")]), "{server}");
    }

    #[test]
    fn unknown_or_unclosed_braces_stay_as_written() {
        assert_eq!(fill("{name} {session}", &VALUES), "{name} work");
        assert_eq!(fill("{{session}}", &VALUES), "{work}");
        assert_eq!(fill("{session", &VALUES), "{session");
        assert_eq!(fill("}{ {server}", &VALUES), "}{ desktop");
        assert_eq!(fill("{日本}{server}", &VALUES), "{日本}desktop");
    }

    #[test]
    fn the_frame_is_the_text_around_the_placeholders() {
        assert_eq!(
            frame("[{session}@{server}]", &VALUES),
            ("[", "{session}@{server}", "]")
        );
        assert_eq!(frame("<< {server} >>", &VALUES), ("<< ", "{server}", " >>"));
        assert_eq!(frame("{session}", &VALUES), ("", "{session}", ""));
        assert_eq!(frame("({other})", &VALUES), ("({other})", "", ""));
    }
}
