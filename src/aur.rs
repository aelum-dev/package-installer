//! The AUR: the RPC request URLs, the reply format, and the plan a request
//! needs. Pure — no GTK, no subprocess, no network. `remote` runs the two
//! commands this describes; `model` decides what a reply means for the
//! system the app is running on.
//!
//! The AUR is not a pacman repository: it carries PKGBUILDs, not packages.
//! Nothing here pretends otherwise — a search answer is a *recipe*, and the
//! only way to install one is to fetch it, build it on this machine, and
//! hand the result to `pacman -U`.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use crate::db::dep_name;

/// The RPC endpoint. Version 5 is the current one; every reply carries its
/// own `version`/`type`, so a server-side change shows up as an error here
/// rather than as an empty list.
pub const RPC: &str = "https://aur.archlinux.org/rpc/";
/// The AUR web host, which is also where the snapshot URLs live.
pub const HOST: &str = "https://aur.archlinux.org";

/// The tools every PKGBUILD build leans on. pacman names them as its own
/// optional dependency (`pacman -Qi pacman`: "base-devel: required to use
/// makepkg"); on Arch it is a metapackage in core whose `%DEPENDS%` is that
/// tool list, which is what `Model::missing_dependencies` reads.
pub const BUILD_TOOLS: &str = "base-devel";

/// The search request for a query. `type=search` answers with names,
/// versions and descriptions — never with dependency lists, which is why
/// installing anything needs an `info` request as well.
pub fn search_url(query: &str) -> String {
    format!("{RPC}?v=5&type=search&arg={}", encode(query))
}

/// The information request for one or more names, including their
/// dependencies. A name the AUR does not have is simply absent from the
/// reply (measured against a request mixing one existing and one missing
/// name: `resultcount` counted the one it had), so a caller must treat a
/// missing name as "no information", never as "no dependencies".
pub fn info_url(names: &[String]) -> String {
    let mut url = format!("{RPC}?v=5&type=info");
    for name in names {
        url.push_str(&format!("&arg[]={}", encode(name)));
    }
    url
}

/// Names per `info` request. The server takes them all in one URL — measured
/// against the live RPC: 200 `arg[]` values (a 4.6 KB request) answer with
/// all 200 packages — so this is a bound for a pathological system, not a
/// limit the AUR imposes at that size.
pub const INFO_BATCH: usize = 100;

/// The names in request-sized groups, order kept.
pub fn info_batches(names: &[String]) -> Vec<Vec<String>> {
    names.chunks(INFO_BATCH).map(|c| c.to_vec()).collect()
}

/// The PKGBUILD snapshot the AUR advertises for a package, from the
/// `URLPath` every reply carries. It is a `.tar.gz` holding
/// `<pkgbase>/PKGBUILD` and the files it references — so fetching one needs
/// no git, only the HTTP client.
pub fn snapshot_url(url_path: &str) -> String {
    if url_path.starts_with('/') {
        format!("{HOST}{url_path}")
    } else {
        format!("{HOST}/{url_path}")
    }
}

/// Percent-encode a query value: the unreserved set of RFC 3986 passes
/// through, every other byte of its UTF-8 form becomes `%XX`.
fn encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// One package in a search answer.
#[derive(Clone, Debug)]
pub struct Hit {
    pub name: String,
    pub version: String,
    pub description: String,
    pub url: String,
    pub url_path: String,
    pub votes: i64,
    pub out_of_date: bool,
}

/// One package in an information answer: what it needs, where its PKGBUILD
/// is, and what it is. The plan reads only the dependency lists; the display
/// fields are there because an `info` answer is read by a person too — the
/// AUR update check asks `type=info` for the names this system has and shows
/// the answer as rows, and an `info` reply carries every field a search
/// answer does (measured against the live RPC: `Version`, `Description`,
/// `URL`, `URLPath`, `NumVotes`, `OutOfDate` are all present).
#[derive(Clone, Debug)]
pub struct Info {
    pub name: String,
    pub url_path: String,
    pub depends: Vec<String>,
    pub make_depends: Vec<String>,
    pub provides: Vec<String>,
    pub version: String,
    pub description: String,
    pub url: String,
    pub votes: i64,
    pub out_of_date: bool,
}

impl Info {
    /// The same answer as a search row, so the model classifies an update
    /// check exactly as it classifies a search — one rule for what "older
    /// than the AUR" means, not two that can drift.
    pub fn as_hit(&self) -> Hit {
        Hit {
            name: self.name.clone(),
            version: self.version.clone(),
            description: self.description.clone(),
            url: self.url.clone(),
            url_path: self.url_path.clone(),
            votes: self.votes,
            out_of_date: self.out_of_date,
        }
    }
}

/// A parsed RPC reply.
#[derive(Debug)]
pub enum Response {
    Search(Vec<Hit>),
    Info(Vec<Info>),
}

/// Read one RPC reply.
///
/// The AUR answers a failed request with **HTTP 200** and a JSON body of its
/// own: `{"type":"error","error":"Too many package results."}` is what a
/// query as common as `python` returns (measured against the live RPC). A
/// caller that only looked at `results` would show "nothing found" for a
/// request the server refused, so an error reply is an error here.
///
/// The order of a search answer is the AUR's own ranking and is kept.
pub fn parse_reply(text: &str) -> Result<Response, String> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("the AUR reply is not JSON: {e}"))?;
    let kind = string(&value, "type");
    if kind == "error" {
        let msg = string(&value, "error");
        return Err(if msg.is_empty() {
            "the AUR refused the request".to_string()
        } else {
            msg
        });
    }
    let results = value.get("results").and_then(|r| r.as_array());
    match kind.as_str() {
        "search" => {
            let hits = results
                .map(|a| a.iter().map(hit).collect::<Vec<_>>())
                .unwrap_or_default();
            Ok(Response::Search(hits))
        }
        // `type=info` answers with `type: "multiinfo"` for several names and
        // `"info"` for one.
        "info" | "multiinfo" => {
            let infos = results
                .map(|a| a.iter().map(info).collect::<Vec<_>>())
                .unwrap_or_default();
            Ok(Response::Info(infos))
        }
        _ => Err(format!("the AUR replied with an unknown type: {kind}")),
    }
}

fn hit(v: &serde_json::Value) -> Hit {
    Hit {
        name: string(v, "Name"),
        version: string(v, "Version"),
        description: string(v, "Description"),
        url: string(v, "URL"),
        url_path: string(v, "URLPath"),
        votes: v.get("NumVotes").and_then(|n| n.as_i64()).unwrap_or(0),
        out_of_date: is_out_of_date(v),
    }
}

fn info(v: &serde_json::Value) -> Info {
    Info {
        name: string(v, "Name"),
        url_path: string(v, "URLPath"),
        depends: strings(v, "Depends"),
        // The RPC's field is `MakeDepends`, not the `.SRCINFO` spelling.
        make_depends: strings(v, "MakeDepends"),
        provides: strings(v, "Provides"),
        version: string(v, "Version"),
        description: string(v, "Description"),
        url: string(v, "URL"),
        votes: v.get("NumVotes").and_then(|n| n.as_i64()).unwrap_or(0),
        out_of_date: is_out_of_date(v),
    }
}

/// `OutOfDate` is `null` for a current package and a unix timestamp for one
/// whose upstream is known to have moved on.
fn is_out_of_date(v: &serde_json::Value) -> bool {
    matches!(v.get("OutOfDate"), Some(serde_json::Value::Number(_)))
}

fn string(v: &serde_json::Value, key: &str) -> String {
    v.get(key)
        .and_then(|s| s.as_str())
        .unwrap_or_default()
        .to_string()
}

fn strings(v: &serde_json::Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(|s| s.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// What an install would have to do about a name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Need {
    /// Already on the system, by name or by an installed provider.
    Satisfied,
    /// A configured repository carries it: `pacman -S` brings it.
    Repository,
    /// The AUR has it: it must be built before the thing that needs it.
    Build,
    /// Nothing here can provide it. Listed to the user rather than guessed at.
    Unknown,
}

/// What one AUR install requires, worked out before anything runs.
#[derive(Clone, Debug, Default)]
pub struct Plan {
    /// AUR packages to build, in the order they must be built: a dependency
    /// comes before whatever needs it.
    pub build: Vec<String>,
    /// Repository packages to install first, in one transaction.
    pub repository: Vec<String>,
    /// Names nothing can satisfy — not installed, not in a repository, not in
    /// the AUR answers. The build will fail; saying so first is the point.
    pub unknown: Vec<String>,
}

/// Work out the plan for installing `requested`.
///
/// `infos` is everything the AUR has said about the names involved (the
/// caller fetches it, in rounds, because a dependency's dependencies are only
/// visible once its own information is asked for). `classify` is the model's
/// answer for one name. A dependency already covered by a package in `build`
/// is not asked for again, and a cycle stops the walk rather than looping —
/// PKGBUILDs that depend on each other are rare, and the first of them will
/// fail with makepkg's own message.
pub fn plan(
    requested: &str,
    infos: &BTreeMap<String, Info>,
    classify: &dyn Fn(&str) -> Need,
) -> Plan {
    let mut plan = Plan::default();
    let mut done: HashSet<String> = HashSet::new();
    let mut path: Vec<String> = Vec::new();
    visit(requested, infos, classify, &mut plan, &mut done, &mut path);
    plan.repository.sort();
    plan.repository.dedup();
    plan.unknown.sort();
    plan.unknown.dedup();
    plan
}

fn visit(
    name: &str,
    infos: &BTreeMap<String, Info>,
    classify: &dyn Fn(&str) -> Need,
    plan: &mut Plan,
    done: &mut HashSet<String>,
    path: &mut Vec<String>,
) {
    if done.contains(name) || path.iter().any(|p| p == name) {
        return;
    }
    path.push(name.to_string());
    if let Some(info) = infos.get(name) {
        for dep in info.depends.iter().chain(info.make_depends.iter()) {
            let dep = dep_name(dep);
            if dep == name || done.contains(dep) {
                continue;
            }
            match classify(dep) {
                Need::Satisfied => {}
                Need::Repository => plan.repository.push(dep.to_string()),
                // A name no repository carries and no answer is *called*
                // can still be something a package we already know about
                // provides — `java-runtime` is spelled that way in the AUR.
                Need::Unknown => match provider(dep, infos, done) {
                    Some(p) => visit(&p, infos, classify, plan, done, path),
                    None => plan.unknown.push(dep.to_string()),
                },
                // The AUR has it: build it first, so the tool that needs it
                // finds it installed.
                Need::Build => visit(dep, infos, classify, plan, done, path),
            }
        }
    }
    path.pop();
    done.insert(name.to_string());
    plan.build.push(name.to_string());
}

/// The first package we know about that provides `dep` under another name.
fn provider(dep: &str, infos: &BTreeMap<String, Info>, done: &HashSet<String>) -> Option<String> {
    infos
        .iter()
        .find(|(name, i)| !done.contains(*name) && i.provides.iter().any(|p| dep_name(p) == dep))
        .map(|(name, _)| name.clone())
}

/// The names one AUR install brought that nothing needs any more — the
/// build-time dependencies, once the finished package is installed.
///
/// `brought` are the names the request put on the system: the repository
/// half and the pkgnames of every built *dependency*. `requested` is never
/// among them — the user asked for that package, so it stays whatever else
/// the answer says. `needed_by` is the system's answer for one name: `None`
/// when the name is not on the system (there is nothing to remove), else the
/// installed packages that still need it — by name or by provision, the rule
/// `-Rns` itself respects.
///
/// What is left was needed by the build alone: makepkg writes a built
/// package's runtime dependencies into its `%DEPENDS%` and its build-only
/// ones nowhere, so a runtime dependency is named by an installed package
/// and a build dependency is named by nothing. `-Rns` may take those.
///
/// The answer is about the set, not one name at a time: a candidate needed
/// only by *another candidate* is still a leftover. `extra-cmake-modules`
/// names `cmake` in its `%DEPENDS%` and both are build-only — asked singly,
/// `cmake` survives through its own grave-mate and stays on the system. A
/// candidate is kept only when a package *outside* the set needs it, and
/// that shrinking is iterated to a fixed point, so a chain that ends in a
/// keeper is kept too.
pub fn unneeded_build_deps(
    requested: &str,
    brought: &[String],
    needed_by: &dyn Fn(&str) -> Option<Vec<String>>,
) -> Vec<String> {
    let mut candidates: BTreeSet<String> = brought
        .iter()
        .filter(|n| n.as_str() != requested)
        .cloned()
        .collect();
    loop {
        let kept: Vec<String> = candidates
            .iter()
            .filter(|n| {
                // Not on the system: nothing to remove. Named by a package
                //    outside the set: it stays.
                needed_by(n).is_none_or(|d| {
                    d.iter().any(|x| !candidates.contains(x.as_str()))
                })
            })
            .cloned()
            .collect();
        if kept.is_empty() {
            break;
        }
        for name in kept {
            candidates.remove(&name);
        }
    }
    candidates.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_encode_the_query_and_the_names() {
        assert_eq!(
            search_url("gtk+-3"),
            "https://aur.archlinux.org/rpc/?v=5&type=search&arg=gtk%2B-3"
        );
        assert_eq!(
            search_url("a b"),
            "https://aur.archlinux.org/rpc/?v=5&type=search&arg=a%20b"
        );
        // A non-ASCII query goes out as UTF-8 bytes.
        assert!(search_url("ä").contains('%'));
        assert_eq!(
            info_url(&["qtengine".into(), "dux-shell".into()]),
            "https://aur.archlinux.org/rpc/?v=5&type=info&arg[]=qtengine&arg[]=dux-shell"
        );
        assert_eq!(
            snapshot_url("/cgit/aur.git/snapshot/qtengine.tar.gz"),
            "https://aur.archlinux.org/cgit/aur.git/snapshot/qtengine.tar.gz"
        );
    }

    /// The shape the live RPC answers with (measured against
    /// `?v=5&type=search&arg=qtengine`).
    const SEARCH: &str = r#"{
      "resultcount": 2,
      "results": [
        {"Description":"QT Platform Theme made easy","FirstSubmitted":1768873006,
         "ID":2321470,"LastModified":1793399573,"Maintainer":"someone","Name":"qtengine",
         "NumVotes":3,"OutOfDate":null,"PackageBase":"qtengine","PackageBaseID":227751,
         "Popularity":1.23643,"URL":"https://github.com/kossLAN/qtengine",
         "URLPath":"/cgit/aur.git/snapshot/qtengine.tar.gz","Version":"0.2.4-1"},
        {"Description":"old theme","ID":1,"Name":"qtengine-old","NumVotes":0,
         "OutOfDate":1700000000,"PackageBase":"qtengine-old","URL":"","Version":"1-1"}
      ],
      "type": "search",
      "version": 5
    }"#;

    const INFO: &str = r#"{
      "resultcount": 1,
      "results": [{
        "Conflicts": ["qtengine-bin"],
        "Depends": ["kconfig", "qt6-base"],
        "Description": "QT Platform Theme made easy",
        "FirstSubmitted": 1768873006, "ID": 2321470, "Keywords": ["qt"],
        "LastModified": 1793399573, "License": ["BSD-2-Clause"],
        "Maintainer": "someone", "MakeDepends": ["cmake", "ninja"],
        "Name": "qtengine", "NumVotes": 3, "OutOfDate": null,
        "PackageBase": "qtengine", "PackageBaseID": 227751, "Popularity": 1.23643,
        "Provides": ["qtengine"], "Submitter": "someone",
        "URL": "https://github.com/kossLAN/qtengine",
        "URLPath": "/cgit/aur.git/snapshot/qtengine.tar.gz", "Version": "0.2.4-1"
      }],
      "type": "multiinfo",
      "version": 5
    }"#;

    #[test]
    fn a_search_reply_becomes_rows_in_the_servers_order() {
        let Response::Search(hits) = parse_reply(SEARCH).unwrap() else {
            panic!("expected a search answer");
        };
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].name, "qtengine");
        assert_eq!(hits[0].version, "0.2.4-1");
        assert_eq!(hits[0].votes, 3);
        assert!(!hits[0].out_of_date);
        // A timestamp in OutOfDate is a real fact about the package.
        assert!(hits[1].out_of_date);
        assert_eq!(
            hits[0].url_path,
            "/cgit/aur.git/snapshot/qtengine.tar.gz"
        );
    }

    #[test]
    fn an_info_reply_carries_both_dependency_lists() {
        let Response::Info(infos) = parse_reply(INFO).unwrap() else {
            panic!("expected an info answer");
        };
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].name, "qtengine");
        assert_eq!(infos[0].provides, ["qtengine"]);
        assert_eq!(infos[0].depends, ["kconfig", "qt6-base"]);
        assert_eq!(infos[0].make_depends, ["cmake", "ninja"]);
        assert_eq!(infos[0].url_path, "/cgit/aur.git/snapshot/qtengine.tar.gz");
    }

    #[test]
    fn an_info_reply_answers_the_same_questions_a_search_answer_does() {
        // The update check asks `type=info` for names this system already
        // has, and its rows are the rows a search draws — so the answer has
        // to carry the same fields, or the two lists would disagree about
        // what a version means.
        let Response::Info(infos) = parse_reply(INFO).unwrap() else {
            panic!("expected an info answer");
        };
        let hit = infos[0].as_hit();
        assert_eq!(hit.name, "qtengine");
        assert_eq!(hit.version, "0.2.4-1");
        assert_eq!(hit.description, "QT Platform Theme made easy");
        assert_eq!(hit.url, "https://github.com/kossLAN/qtengine");
        assert_eq!(hit.votes, 3);
        assert!(!hit.out_of_date);
        assert_eq!(hit.url_path, "/cgit/aur.git/snapshot/qtengine.tar.gz");
    }

    #[test]
    fn an_info_request_stays_inside_its_batch() {
        let names: Vec<String> = (0..250).map(|i| format!("pkg{i}")).collect();
        let batches = info_batches(&names);
        assert_eq!(batches.iter().map(Vec::len).collect::<Vec<_>>(), [100, 100, 50]);
        // Order is the caller's; nothing is dropped or asked twice.
        assert_eq!(batches.concat(), names);
        assert!(info_batches(&[]).is_empty());
    }

    #[test]
    fn a_refused_request_is_an_error_not_an_empty_list() {
        // Measured: `arg=python` and `arg=git` both answer this, with HTTP
        // 200 and an empty results array. Rendering it as "nothing found"
        // would be the app lying about a search it never ran.
        let err = parse_reply(
            r#"{"error":"Too many package results.","resultcount":0,"results":[],"type":"error","version":5}"#,
        )
        .unwrap_err();
        assert_eq!(err, "Too many package results.");
        // An unknown type is the same kind of failure.
        assert!(parse_reply(r#"{"type":"nonsense","results":[]}"#).is_err());
        assert!(parse_reply("not json").is_err());
    }

    /// `(name, depends, makedepends, provides)`.
    type Spec<'a> = (&'a str, &'a [&'a str], &'a [&'a str], &'a [&'a str]);
    fn infos(list: &[Spec]) -> BTreeMap<String, Info> {
        let mut map = BTreeMap::new();
        for (name, depends, make, provides) in list {
            map.insert(
                (*name).to_string(),
                Info {
                    name: (*name).to_string(),
                    url_path: format!("/cgit/aur.git/snapshot/{name}.tar.gz"),
                    depends: depends.iter().map(|s| s.to_string()).collect(),
                    make_depends: make.iter().map(|s| s.to_string()).collect(),
                    provides: provides.iter().map(|s| s.to_string()).collect(),
                    // A plan reads none of these; the reply test covers them.
                    version: String::new(),
                    description: String::new(),
                    url: String::new(),
                    votes: 0,
                    out_of_date: false,
                },
            );
        }
        map
    }

    /// The system the caller would report: `glibc` installed, `cmake` and
    /// `ninja` in a repository, and — the rule `remote` really uses — a name
    /// is buildable only when the AUR has answered something about it.
    fn classify(infos: &BTreeMap<String, Info>) -> impl Fn(&str) -> Need {
        move |name| match name {
            "glibc" => Need::Satisfied,
            "cmake" | "ninja" => Need::Repository,
            _ if infos.contains_key(name) => Need::Build,
            _ => Need::Unknown,
        }
    }

    #[test]
    fn a_plan_builds_dependencies_before_the_package_that_needs_them() {
        let map = infos(&[
            ("myapp", &["glibc", "helper"], &["cmake"], &[]),
            ("helper", &["glibc"], &[], &[]),
        ]);
        let p = plan("myapp", &map, &classify(&map));
        assert_eq!(p.build, ["helper", "myapp"]);
        assert_eq!(p.repository, ["cmake"]);
        assert!(p.unknown.is_empty());
    }

    #[test]
    fn a_dependency_another_build_provides_is_not_asked_for_twice() {
        // helper is installed by the time myapp builds, so its own
        // dependencies are not in the repository list.
        let map = infos(&[
            ("myapp", &["helper"], &["ninja"], &[]),
            ("helper", &["ninja"], &[], &[]),
        ]);
        let p = plan("myapp", &map, &classify(&map));
        assert_eq!(p.build, ["helper", "myapp"]);
        assert_eq!(p.repository, ["ninja"], "one transaction, one entry");
    }

    #[test]
    fn an_unresolvable_dependency_is_named_instead_of_guessed_at() {
        let map = infos(&[("myapp", &["glibc", "nowhere"], &[], &[])]);
        let p = plan("myapp", &map, &classify(&map));
        assert_eq!(p.build, ["myapp"]);
        assert_eq!(p.unknown, ["nowhere"]);
    }

    #[test]
    fn a_dependency_something_else_provides_is_not_left_unknown() {
        // The AUR spells a lot of dependencies as provisions. `jdk` answers
        // with Name=jdk and Provides=java-runtime; the plan has to build jdk,
        // not report an impossible dependency.
        let map = infos(&[
            ("myapp", &["java-runtime"], &[], &[]),
            ("jdk", &[], &[], &["java-runtime"]),
        ]);
        let p = plan("myapp", &map, &classify(&map));
        assert_eq!(p.build, ["jdk", "myapp"]);
        assert!(p.unknown.is_empty());
    }

    #[test]
    fn a_dependency_cycle_does_not_loop() {
        let map = infos(&[("a", &["b"], &[], &[]), ("b", &["a"], &[], &[])]);
        let p = plan("a", &map, &classify(&map));
        assert_eq!(p.build, ["b", "a"]);
    }

    #[test]
    fn a_package_with_no_information_is_still_the_thing_being_built() {
        // The caller asks for information in rounds; a name it has not
        // received an answer for has no known dependencies, but it is still
        // the package the user asked for.
        let p = plan("lonely", &BTreeMap::new(), &classify(&BTreeMap::new()));
        assert_eq!(p.build, ["lonely"]);
        assert!(p.repository.is_empty() && p.unknown.is_empty());
    }

    #[test]
    fn names_only_the_build_needed_are_left_after_the_install() {
        // myapp needs kconfig at runtime and cmake to build; helper is
        // built and needed at runtime; tool is built and needed only to
        // build. The system the caller answers with: an installed package
        // (myapp) names kconfig and helper in its %DEPENDS% and names cmake
        // and tool nowhere.
        let brought = ["kconfig", "cmake", "helper", "tool", "myapp"]
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>();
        let needed = |n: &str| match n {
            "kconfig" | "helper" => Some(vec!["myapp".to_string()]),
            "cmake" | "tool" => Some(vec![]),
            _ => None,
        };
        let left = unneeded_build_deps("myapp", &brought, &needed);
        assert_eq!(left, ["cmake", "tool"]);
    }

    #[test]
    fn the_requested_package_and_absent_names_are_never_left_over() {
        // myapp is not needed by anything either — it is what the user
        // asked for, so it stays. A name that is not on the system has
        // nothing to remove.
        let brought = ["myapp", "cmake", "gone", "cmake"]
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>();
        let needed = |n: &str| match n {
            "cmake" => Some(vec![]),
            _ => None,
        };
        let left = unneeded_build_deps("myapp", &brought, &needed);
        assert_eq!(left, ["cmake"], "sorted and deduplicated");
    }

    #[test]
    fn leftovers_that_need_each_other_go_too() {
        // Measured on a real qtengine install: its makedepend
        // extra-cmake-modules names cmake in its %DEPENDS%, and both were
        // build-only. Asked one name at a time, cmake survives through its
        // own grave-mate; about the set, both go.
        let brought = ["cmake", "extra-cmake-modules", "ninja", "qtengine"]
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>();
        let needed = |n: &str| match n {
            "cmake" => Some(vec!["extra-cmake-modules".to_string()]),
            "extra-cmake-modules" | "ninja" => Some(vec![]),
            _ => None,
        };
        let left = unneeded_build_deps("qtengine", &brought, &needed);
        assert_eq!(left, ["cmake", "extra-cmake-modules", "ninja"]);
    }

    #[test]
    fn a_chain_into_a_keeper_is_kept() {
        // qt6-base is needed by kconfig, kconfig by the requested qtengine.
        // kconfig is kept on the first pass, and that makes qt6-base needed
        // by something outside the set on the next — the fixed point is
        // what -Rns would resolve to.
        let brought = ["qt6-base", "kconfig", "qtengine"]
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>();
        let needed = |n: &str| match n {
            "qt6-base" => Some(vec!["kconfig".to_string()]),
            "kconfig" => Some(vec!["qtengine".to_string()]),
            _ => None,
        };
        let left = unneeded_build_deps("qtengine", &brought, &needed);
        assert!(left.is_empty());
    }
}
