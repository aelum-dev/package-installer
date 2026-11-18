//! The countries a mirror update can be aimed at: the list, the fuzzy search,
//! and the default a system's own files point at.
//!
//! Pure: no GTK, no process, no file reads — callers hand in the file
//! contents, so every decision here is testable without an installed system.
//!
//! `COUNTRIES` is the installer's region table
//! (`../installer/backend/shared/regions_table.go`) reduced to its two
//! mirror-facing fields. The name is exactly as Arch's mirrorlist spells the
//! country's section and the code is the ISO 3166-1 alpha-2 that section maps
//! to — measured against reflector 2023 (the `[extra]` version), those are the
//! two things `--country` matches, case-insensitively, by
//! `country`/`country_code` against the live mirror-status database. So the
//! app can send the plain ISO code and be immune to a display name's spelling
//! ("Türkiye" and friends still work, matched by code).
//!
//! The worldwide entry is first, with the installer's own `DefaultRegionCode`
//! as its code: it names the same location the installer's location page
//! offers, and selecting it means `--country` is not passed at all — all
//! Arch mirrors, still rate-sorted. A country entry whose mirror is
//! momentarily inactive is no hazard: reflector exits `error: no mirrors
//! found` before it touches the save file, and the dialog shows that.

/// One selectable country.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Country {
    /// ISO 3166-1 alpha-2, lowercase, as the mirrorlist maps it; the
    /// worldwide entry carries the installer's `DefaultRegionCode` instead.
    pub code: &'static str,
    /// Exactly as Arch's mirrorlist spells this country's section — the name
    /// English-speaking users recognise on a map, untranslated like every
    /// other proper noun this app shows.
    pub name: &'static str,
}

/// The installer's worldwide location: every Arch mirror, no country filter.
pub const WORLDWIDE_CODE: &str = "worldwide";

/// The options, worldwide first, then every mirror country by name — the
/// installer's own order (`GetRegions` sorts by lowercase name).
pub const COUNTRIES: &[Country] = &[
    Country {
        code: WORLDWIDE_CODE,
        name: "Worldwide",
    },
    Country {
        code: "al",
        name: "Albania",
    },
    Country {
        code: "ar",
        name: "Argentina",
    },
    Country {
        code: "am",
        name: "Armenia",
    },
    Country {
        code: "au",
        name: "Australia",
    },
    Country {
        code: "at",
        name: "Austria",
    },
    Country {
        code: "az",
        name: "Azerbaijan",
    },
    Country {
        code: "bd",
        name: "Bangladesh",
    },
    Country {
        code: "be",
        name: "Belgium",
    },
    Country {
        code: "br",
        name: "Brazil",
    },
    Country {
        code: "bg",
        name: "Bulgaria",
    },
    Country {
        code: "kh",
        name: "Cambodia",
    },
    Country {
        code: "ca",
        name: "Canada",
    },
    Country {
        code: "cl",
        name: "Chile",
    },
    Country {
        code: "cn",
        name: "China",
    },
    Country {
        code: "co",
        name: "Colombia",
    },
    Country {
        code: "cz",
        name: "Czechia",
    },
    Country {
        code: "dk",
        name: "Denmark",
    },
    Country {
        code: "ec",
        name: "Ecuador",
    },
    Country {
        code: "ee",
        name: "Estonia",
    },
    Country {
        code: "fi",
        name: "Finland",
    },
    Country {
        code: "fr",
        name: "France",
    },
    Country {
        code: "ge",
        name: "Georgia",
    },
    Country {
        code: "de",
        name: "Germany",
    },
    Country {
        code: "gr",
        name: "Greece",
    },
    Country {
        code: "hk",
        name: "Hong Kong",
    },
    Country {
        code: "hu",
        name: "Hungary",
    },
    Country {
        code: "is",
        name: "Iceland",
    },
    Country {
        code: "in",
        name: "India",
    },
    Country {
        code: "id",
        name: "Indonesia",
    },
    Country {
        code: "ir",
        name: "Iran",
    },
    Country {
        code: "il",
        name: "Israel",
    },
    Country {
        code: "it",
        name: "Italy",
    },
    Country {
        code: "jp",
        name: "Japan",
    },
    Country {
        code: "kz",
        name: "Kazakhstan",
    },
    Country {
        code: "ke",
        name: "Kenya",
    },
    Country {
        code: "lv",
        name: "Latvia",
    },
    Country {
        code: "lt",
        name: "Lithuania",
    },
    Country {
        code: "lu",
        name: "Luxembourg",
    },
    Country {
        code: "my",
        name: "Malaysia",
    },
    Country {
        code: "mu",
        name: "Mauritius",
    },
    Country {
        code: "mx",
        name: "Mexico",
    },
    Country {
        code: "md",
        name: "Moldova",
    },
    Country {
        code: "ma",
        name: "Morocco",
    },
    Country {
        code: "np",
        name: "Nepal",
    },
    Country {
        code: "nl",
        name: "Netherlands",
    },
    Country {
        code: "nc",
        name: "New Caledonia",
    },
    Country {
        code: "nz",
        name: "New Zealand",
    },
    Country {
        code: "mk",
        name: "North Macedonia",
    },
    Country {
        code: "no",
        name: "Norway",
    },
    Country {
        code: "pl",
        name: "Poland",
    },
    Country {
        code: "pt",
        name: "Portugal",
    },
    Country {
        code: "ro",
        name: "Romania",
    },
    Country {
        code: "ru",
        name: "Russia",
    },
    Country {
        code: "sa",
        name: "Saudi Arabia",
    },
    Country {
        code: "rs",
        name: "Serbia",
    },
    Country {
        code: "sg",
        name: "Singapore",
    },
    Country {
        code: "sk",
        name: "Slovakia",
    },
    Country {
        code: "si",
        name: "Slovenia",
    },
    Country {
        code: "za",
        name: "South Africa",
    },
    Country {
        code: "kr",
        name: "South Korea",
    },
    Country {
        code: "es",
        name: "Spain",
    },
    Country {
        code: "se",
        name: "Sweden",
    },
    Country {
        code: "ch",
        name: "Switzerland",
    },
    Country {
        code: "tw",
        name: "Taiwan",
    },
    Country {
        code: "th",
        name: "Thailand",
    },
    Country {
        code: "tn",
        name: "Tunisia",
    },
    Country {
        code: "tr",
        name: "Türkiye",
    },
    Country {
        code: "ua",
        name: "Ukraine",
    },
    Country {
        code: "ae",
        name: "United Arab Emirates",
    },
    Country {
        code: "gb",
        name: "United Kingdom",
    },
    Country {
        code: "us",
        name: "United States",
    },
    Country {
        code: "uz",
        name: "Uzbekistan",
    },
    Country {
        code: "vn",
        name: "Vietnam",
    },
];

/// Index of a 2-letter ISO code (any case), if the table has it.
pub fn index_of_code(code: &str) -> Option<usize> {
    let code = code.to_ascii_lowercase();
    COUNTRIES.iter().position(|c| c.code == code)
}

/// Index of a country named exactly as the mirrorlist spells it, case- and
/// accent-insensitively — how the `## Arch Linux mirrorlist for <Name>`
/// header's value comes back.
pub fn index_of_name(name: &str) -> Option<usize> {
    let needle = fold_key(name);
    COUNTRIES.iter().position(|c| fold_key(c.name) == needle)
}

/// Which entry a system's current configuration points at. First its
/// mirrorlist: reflector records the command it ran in the header of the file
/// it saves, and before that the installer's own header names the location the
/// install chose. Then the territory of the system locale — the installer
/// pre-fills location from language, so for the common case that is the
/// install's answer. Else Worldwide.
pub fn default_index(mirrorlist: Option<&str>, locale_conf: Option<&str>) -> usize {
    mirrorlist
        .and_then(mirrorlist_index)
        .or_else(|| locale_conf.and_then(locale_index))
        .unwrap_or(0)
}

/// The country a mirrorlist was last generated for: reflector's `# With:`
/// header line first (a `--country` token — the code we send, or a name a
/// hand-run reflector used), then the installer's header.
fn mirrorlist_index(content: &str) -> Option<usize> {
    let mut installer_header = None;
    for line in content.lines() {
        let line = line.trim_start();
        if let Some(rest) = line.strip_prefix("# With:") {
            if let Some(index) = reflector_country(rest) {
                return Some(index);
            }
        } else if let Some(rest) = line.strip_prefix("## Arch Linux mirrorlist for ") {
            installer_header = installer_country(rest.trim());
        }
        // The installer's header is the last word: it sits above every Server
        // line, and reflector's own header is checked as it is seen.
        if line.starts_with("Server = ") {
            break;
        }
    }
    installer_header
}

/// The `--country` value of a reflector command line: a comma list takes its
/// first entry, a quoted name is reassembled from the whitespace-split tokens
/// (`shlex.quote` puts `'United States'` on the line).
fn reflector_country(command: &str) -> Option<usize> {
    let mut args = command.split_whitespace();
    let mut value = String::new();
    while let Some(arg) = args.next() {
        if arg != "--country" {
            continue;
        }
        let token = args.next()?;
        value.push_str(token);
        while value.starts_with('\'') && !value.ends_with('\'') {
            value.push(' ');
            value.push_str(args.next()?);
        }
        break;
    }
    if value.is_empty() {
        return None;
    }
    let value = value.trim_matches('\'').trim_matches('"');
    let first = value.split(',').next()?;
    if is_iso_code(first) {
        index_of_code(first)
    } else {
        index_of_name(first)
    }
}

/// The header line's value: `<Name>`, or `<Name> (<code>)` in the generator's
/// own per-country files; both shapes resolve.
fn installer_country(rest: &str) -> Option<usize> {
    if let Some((name, code)) = rest.rsplit_once('(') {
        let (name, code) = (name.trim_end(), code.trim_end_matches(')'));
        // The code decides when the header carries one; the name answers for
        // every header that does not.
        if is_iso_code(code) {
            return index_of_code(code).or_else(|| index_of_name(name));
        }
        return index_of_name(name);
    }
    index_of_name(rest)
}

/// The territory of a `/etc/locale.conf` — the installer writes exactly one
/// `LANG=` line ("LANG=de_DE.UTF-8\n"); the first one wins like systemd's
/// reader's. A language with no territory ("eo") points nowhere.
fn locale_index(content: &str) -> Option<usize> {
    let value = content
        .lines()
        .find_map(|line| line.trim().strip_prefix("LANG="))?;
    let base = value
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .split(['.', '@'])
        .next()?;
    let territory = base.rsplit('_').next().filter(|t| is_iso_code(t))?;
    index_of_code(territory)
}

fn is_iso_code(value: &str) -> bool {
    value.len() == 2 && value.bytes().all(|b| b.is_ascii_alphabetic())
}

/// The fuzzy match behind the picker: every query character appears in the
/// name, in order, gaps allowed — case- and accent-insensitive, whitespace
/// ignored, so "gma" finds Germany, "un sta" finds United States, and
/// "turkiye" finds Türkiye. The exact ISO code is a whole-query alternative,
/// so "us" finds United States and nothing else.
pub fn fuzzy_match(query: &str, country: &Country) -> bool {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return true;
    }
    if fold_key(trimmed) == country.code {
        return true;
    }
    let mut needle = trimmed.chars().map(fold).filter(|c| !c.is_whitespace());
    let name = fold_key(country.name);
    let mut hay = name.chars().filter(|c| !c.is_whitespace());
    loop {
        match needle.next() {
            None => return true, // every needle character was found, in order
            Some(want) => loop {
                match hay.next() {
                    None => return false, // the name ran out mid-needle
                    Some(have) if have == want => break,
                    Some(_) => continue,
                }
            },
        }
    }
}

/// The table, in order, as (index, entry) pairs a query matches — the index
/// travels along because the picker's selection is an index, and a filtered
/// list must still be able to say which country a click chose.
pub fn search(query: &str) -> Vec<(usize, &'static Country)> {
    COUNTRIES
        .iter()
        .enumerate()
        .filter(|(_, country)| fuzzy_match(query, country))
        .collect()
}

/// Lowercase with the Latin-1 and Latin-A accents stripped: query and name are
/// compared folded, so accents are typed loosely rather than demanded. The
/// set covers the letters the mirror countries' names actually carry today —
/// Türkiye is the only non-ASCII one — plus their neighbours, so a table that
/// grows an accented name still types loosely.
fn fold(c: char) -> char {
    let c = c.to_lowercase().next().unwrap_or(c);
    match c {
        'a'..='z' | '0'..='9' => c,
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => 'a',
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => 'c',
        'ď' | 'đ' => 'd',
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => 'e',
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => 'g',
        'ĥ' => 'h',
        'ì' | 'í' | 'î' | 'ï' | 'ī' | 'ĭ' | 'į' | 'ı' => 'i',
        'ĵ' => 'j',
        'ķ' | 'ĸ' => 'k',
        'ł' | 'ĺ' | 'ļ' | 'ŀ' => 'l',
        'ñ' | 'ń' | 'ņ' | 'ŉ' => 'n',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => 'o',
        'ŕ' | 'ř' | 'ŗ' => 'r',
        'ś' | 'ŝ' | 'ş' | 'ș' | 'š' => 's',
        'ť' | 'ţ' | 'ț' | 'ŧ' => 't',
        'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => 'u',
        'ŵ' => 'w',
        'ý' | 'ÿ' | 'ȳ' => 'y',
        'ź' | 'ż' | 'ž' => 'z',
        other => other,
    }
}

fn fold_key(value: &str) -> String {
    value.chars().map(fold).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_worldwide_first_then_sorted_and_unique() {
        let (worldwide, rest) = COUNTRIES.split_first().expect("the table is not empty");
        assert_eq!(worldwide.code, WORLDWIDE_CODE);
        assert_eq!(worldwide.name, "Worldwide");
        let mut previous = String::new();
        for country in rest {
            assert!(
                country.code.len() == 2 && country.code.bytes().all(|b| b.is_ascii_lowercase()),
                "{} has a code that is not a lowercase ISO pair",
                country.code
            );
            let name = country.name.to_lowercase();
            assert!(
                name >= previous,
                "{} is out of alphabetical order",
                country.name
            );
            previous = name;
        }
        let mut codes: Vec<_> = COUNTRIES.iter().map(|c| c.code).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), COUNTRIES.len(), "two countries share a code");
        let mut names: Vec<_> = COUNTRIES.iter().map(|c| c.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), COUNTRIES.len(), "two countries share a name");
    }

    #[test]
    fn lookup_accepts_what_the_two_headers_and_the_locale_may_carry() {
        assert_eq!(
            index_of_code("us"),
            COUNTRIES.iter().position(|c| c.name == "United States")
        );
        assert_eq!(index_of_code("US"), index_of_code("us"));
        assert_eq!(index_of_code("zz"), None);
        assert_eq!(index_of_name("Ukraine"), index_of_code("ua"));
        assert_eq!(index_of_name("ukraine"), index_of_name("Ukraine"));
        assert_eq!(index_of_name("Türkiye"), index_of_code("tr"));
        assert_eq!(index_of_name("Türkiye"), index_of_name("turkiye"));
        assert_eq!(index_of_name("Nowhere"), None);
    }

    #[test]
    fn fuzzy_finds_a_name_through_typos_gaps_and_accents() {
        let germany = COUNTRIES.iter().find(|c| c.name == "Germany").unwrap();
        let united_states = COUNTRIES
            .iter()
            .find(|c| c.name == "United States")
            .unwrap();
        let turkiye = COUNTRIES.iter().find(|c| c.code == "tr").unwrap();
        assert!(fuzzy_match("", germany));
        assert!(fuzzy_match("  ", germany)); // a blank query is all of them
        assert!(fuzzy_match("gma", germany));
        assert!(fuzzy_match("GEM", germany));
        assert!(fuzzy_match("un sta", united_states));
        assert!(fuzzy_match("unitedstates", united_states)); // spaces are free
        assert!(fuzzy_match("turkiye", turkiye)); // accents are free too
        assert!(fuzzy_match("Türkiye", turkiye)); // and may be typed
        // The exact ISO code matches whatever the name letters do. Short
        // queries are broad by design — "de" is a subsequence of "Denmark"
        // and "United States" as well; they appear beside Germany, never
        // instead of it. A subsequence never invents letters:
        assert!(fuzzy_match("de", germany));
        // A word with no route through the name in order does not match:
        // "yam" needs an a after the y, and Germany has none (and "many" is
        // a literal suffix of Germany, so the old negative was luck).
        assert!(!fuzzy_match("yam", germany));
        assert!(!fuzzy_match("zz", germany));
    }

    #[test]
    fn search_keeps_the_table_order() {
        let names: Vec<&str> = search("united").into_iter().map(|(_, c)| c.name).collect();
        assert_eq!(
            names,
            vec!["United Arab Emirates", "United Kingdom", "United States"]
        );
        assert_eq!(search("").len(), COUNTRIES.len());
        assert!(search("qqqq").is_empty());
        // The index is the row's meaning: the filtered entry answers with the
        // position the picker's selection must take.
        let (index, country) = search("Ukraine")[0];
        assert_eq!(
            (index, country.name),
            (countries_index("Ukraine"), "Ukraine")
        );
    }

    fn countries_index(name: &str) -> usize {
        COUNTRIES.iter().position(|c| c.name == name).unwrap()
    }

    #[test]
    fn a_reflector_mirrorlist_names_the_country_it_was_generated_for() {
        // The real shape of the file reflector --save writes, header verbatim
        // (format_mirrorlist's `# With:` line padded to 11 columns).
        let saved = "\
###############################################################################
################# Arch Linux mirrorlist generated by Reflector ################
###############################################################################

# With:       reflector --latest 20 --protocol https --country ua --sort rate --save /etc/pacman.d/mirrorlist
# When:       2026-11-18 09:00:00 UTC
# From:       https://archlinux.org/mirrors/status/json/
# Retrieved:  2026-11-18 09:00:00 UTC
# Last Check: 2026-11-18 08:54:31 UTC

Server = https://mirror.example.ua/archlinux/$repo/os/$arch
";
        assert_eq!(mirrorlist_index(saved), index_of_code("ua"));
        // A hand-run reflector with a quoted name still resolves.
        let named = saved.replace("--country ua", "--country 'United States'");
        assert_eq!(mirrorlist_index(&named), index_of_code("us"));
        // A comma list takes its first entry — reflector's country priority.
        let listed = saved.replace("--country ua", "--country se,no");
        assert_eq!(mirrorlist_index(&listed), index_of_code("se"));
        // A country this app does not offer names no index; the caller falls
        // through to the locale rather than guessing.
        let foreign = saved.replace("--country ua", "--country by");
        assert_eq!(mirrorlist_index(&foreign), None);
        // No --country at all is the worldwide run.
        let all = saved.replace("--country ua ", "");
        assert_eq!(mirrorlist_index(&all), None);
    }

    #[test]
    fn an_installer_mirrorlist_names_the_location_the_install_chose() {
        // BuildMirrorList's headerFor, the file an Aelum install boots with.
        let installed = "\
##
## Arch Linux mirrorlist for Ukraine
##
## Mirrors in Ukraine first, then the worldwide mirrors as a fallback for when
## one of them is down. Written by the Aelum installer.
##

#Server = https://mirror.example.ua/archlinux/$repo/os/$arch
Server = https://mirror.example2.ua/archlinux/$repo/os/$arch
";
        assert_eq!(mirrorlist_index(installed), index_of_code("ua"));
        let worldwide = "##\n## Arch Linux mirrorlist for Worldwide\n##\n\nServer = x\n";
        assert_eq!(mirrorlist_index(worldwide), index_of_code("worldwide"));
        // The generator's per-country files name the code too.
        let section = "##\n## Arch Linux mirrorlist for Germany (de)\n##\n\n#Server = x\n";
        assert_eq!(mirrorlist_index(section), index_of_code("de"));
        // A mirrorlist that is neither's — pacman-mirrorlist's stock file —
        // says nothing: no guess.
        let stock = "##\n## Arch Linux Repository Mirrorlist\n##\n\n## Germany\nServer = x\n";
        assert_eq!(mirrorlist_index(stock), None);
    }

    #[test]
    fn reflectors_record_beats_the_installers_header() {
        // The chain is "most recent answer first": a reflector run is a newer
        // decision than the install-time header, whatever line order the file
        // holds them in.
        let saved = "# With:       reflector --latest 20 --country us --sort rate --save /etc/pacman.d/mirrorlist\n"
            .to_owned()
            + "## Arch Linux mirrorlist for Ukraine\nServer = x\n";
        assert_eq!(mirrorlist_index(&saved), index_of_code("us"));
    }

    #[test]
    fn the_locale_points_at_its_territory() {
        assert_eq!(locale_index("LANG=de_DE.UTF-8\n"), index_of_code("de"));
        assert_eq!(locale_index("LANG=en_US.UTF-8\n"), index_of_code("us"));
        assert_eq!(locale_index("LANG=\"pt_BR.UTF-8\"\n"), index_of_code("br"));
        assert_eq!(locale_index("LANG=sr_RS\n"), index_of_code("rs"));
        // No territory means no country; a modifier is not one either.
        assert_eq!(locale_index("LANG=eo\n"), None);
        assert_eq!(locale_index("LANG=C.UTF-8\n"), None);
        assert_eq!(locale_index("LOCALE_ARCHIVE=/x\n"), None);
    }

    #[test]
    fn the_default_chain_is_mirrorlist_then_locale_then_worldwide() {
        let saved = "# With:       reflector --latest 20 --country ua --sort rate --save /x\n\nServer = x\n";
        let installed = "## Arch Linux mirrorlist for Germany\n\nServer = x\n";
        let locale = "LANG=fr_FR.UTF-8\n";
        assert_eq!(
            default_index(Some(saved), Some(locale)),
            index_of_code("ua").unwrap()
        );
        assert_eq!(
            default_index(Some(installed), Some(locale)),
            index_of_code("de").unwrap()
        );
        assert_eq!(
            default_index(Some("Server = x\n"), Some(locale)),
            index_of_code("fr").unwrap()
        );
        assert_eq!(default_index(None, None), 0);
        assert_eq!(COUNTRIES[default_index(None, None)].code, WORLDWIDE_CODE);
    }

    #[test]
    #[ignore = "reads the installer repo; runs only when it sits beside this one"]
    fn the_table_is_the_installers_region_table() {
        // The oracle for every (code, name) pair: the table that decides what
        // a real install writes, next door in the distro workspace. A new
        // mirror country appears here as a failing line, never as a silent
        // absence.
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../installer/backend/shared/regions_table.go"
        );
        let Ok(source) = std::fs::read_to_string(path) else {
            eprintln!("skipped: no installer tree at {path}");
            return;
        };
        let pairs: Vec<(String, String)> = source
            .lines()
            .filter_map(|line| {
                let start = line.find("{Code: \"")? + 8;
                let (code, rest) = line[start..].split_once("\", Name: \"")?;
                let name = rest.split_once("\",")?.0;
                Some((code.to_owned(), name.to_owned()))
            })
            .filter(|(code, _)| code != WORLDWIDE_CODE)
            .collect();
        let ours: Vec<(String, String)> = COUNTRIES
            .iter()
            .filter(|c| c.code != WORLDWIDE_CODE)
            .map(|c| (c.code.to_owned(), c.name.to_owned()))
            .collect();
        assert_eq!(
            ours, pairs,
            "the country table drifted from the installer's"
        );
    }
}
