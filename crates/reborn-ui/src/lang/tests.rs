use super::*;

fn placeholders(text: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find('{') {
        let Some(len) = rest[start..].find('}') else {
            break;
        };
        found.push(&rest[start..=start + len]);
        rest = &rest[start + len + 1..];
    }
    found.sort_unstable();
    found
}

#[test]
fn every_table_is_sorted_unique_and_keeps_the_placeholders_of_its_english() {
    for language in Language::ALL {
        let Some(table) = table(language) else {
            continue;
        };
        for pair in table.windows(2) {
            assert!(
                pair[0].0 < pair[1].0,
                "{:?}: {:?} before {:?}",
                language,
                pair[0].0,
                pair[1].0
            );
        }
        for (english, translated) in table {
            assert!(
                !translated.is_empty(),
                "{language:?}: {english:?} has no text"
            );
            assert_eq!(
                placeholders(english),
                placeholders(translated),
                "{language:?}: {english:?} and its translation differ in {{}} placeholders"
            );
        }
    }
}

#[test]
fn a_missing_translation_is_the_english_text() {
    assert_eq!(tr_in(Language::English, "Language"), "Language");
    assert_eq!(tr_in(Language::Czech, "Language"), "Jazyk");
    assert_eq!(
        tr_in(Language::Czech, "A text no table has"),
        "A text no table has"
    );
}

#[test]
fn plural_forms_follow_each_language() {
    let czech = |n| plural_in(Language::Czech, n, "skladba", "skladby", "skladeb");
    assert_eq!(
        [0, 1, 2, 4, 5, 21, 100].map(czech),
        ["skladeb", "skladba", "skladby", "skladby", "skladeb", "skladeb", "skladeb"]
    );
    let english = |n| plural_in(Language::English, n, "song", "songs", "songs");
    assert_eq!([0, 1, 2].map(english), ["songs", "song", "songs"]);
}

#[test]
fn the_chosen_language_round_trips() {
    for language in Language::ALL {
        set(language);
        assert_eq!(current(), language);
    }
    set(Language::English);
}
