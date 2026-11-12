//! Classifier: the ordered rule table from DESIGN.md section 8.
//!
//! Pure code: no I/O, no clocks, no process spawning. The first matching rule
//! wins, so frameworks (which run on generic runtimes) come before runtimes.
//! Adding a framework = one entry in [`RULES`] + one test case.

use crate::model::Kind;

/// What a rule looks at. All comparisons are case-insensitive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Match {
    /// The argv contains this word (or these consecutive words), compared
    /// token by token; see [`argv_matches`].
    ArgvTokens(&'static str),
    /// The process name equals this string.
    NameEquals(&'static str),
    /// The process name starts with this string.
    NameStartsWith(&'static str),
}

/// One row of the classification table. Patterns are lowercase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rule {
    /// Priority group from the spec table (1 = frontend ... 6 = runtimes).
    pub group: u8,
    pub pattern: Match,
    pub kind: Kind,
}

const fn argv(group: u8, needle: &'static str, kind: Kind) -> Rule {
    Rule {
        group,
        pattern: Match::ArgvTokens(needle),
        kind,
    }
}

const fn name_is(group: u8, name: &'static str, kind: Kind) -> Rule {
    Rule {
        group,
        pattern: Match::NameEquals(name),
        kind,
    }
}

const fn name_starts(group: u8, prefix: &'static str, kind: Kind) -> Rule {
    Rule {
        group,
        pattern: Match::NameStartsWith(prefix),
        kind,
    }
}

/// The rule table, in priority order. Group 7 (fallback) is implicit:
/// no match means [`Kind::Other`].
pub const RULES: &[Rule] = &[
    // 1. Frontend frameworks: argv tokens
    argv(1, "vite", Kind::Vite),
    argv(1, "next dev", Kind::NextJs),
    argv(1, "next-server", Kind::NextJs),
    argv(1, "webpack", Kind::Webpack),
    argv(1, "react-scripts", Kind::ReactScripts),
    argv(1, "astro", Kind::Astro),
    argv(1, "nuxt", Kind::Nuxt),
    argv(1, "expo", Kind::Expo),
    argv(1, "metro", Kind::Metro),
    // 2. Backend frameworks: argv tokens
    argv(2, "manage.py runserver", Kind::Django),
    argv(2, "uvicorn", Kind::FastApi),
    argv(2, "flask", Kind::Flask),
    argv(2, "gunicorn", Kind::Gunicorn),
    argv(2, "rails", Kind::Rails),
    argv(2, "puma", Kind::Puma),
    argv(2, "artisan serve", Kind::Laravel),
    argv(2, "spring", Kind::Spring),
    // 3. Tools: argv tokens
    argv(3, "jupyter", Kind::Jupyter),
    argv(3, "streamlit", Kind::Streamlit),
    argv(3, "http.server", Kind::PythonHttp),
    argv(3, "ollama", Kind::Ollama),
    // 4. Databases / infra: process name equals
    name_is(4, "postgres", Kind::Postgres),
    name_is(4, "mysqld", Kind::Mysql),
    name_is(4, "mariadbd", Kind::Mariadb),
    name_is(4, "redis-server", Kind::Redis),
    name_is(4, "mongod", Kind::Mongodb),
    name_is(4, "nginx", Kind::Nginx),
    name_is(4, "caddy", Kind::Caddy),
    // 5. Container plumbing: process name equals
    name_is(5, "docker-proxy", Kind::Docker),
    name_is(5, "com.docker.backend", Kind::Docker),
    name_is(5, "vpnkit", Kind::Docker),
    name_is(5, "wslrelay", Kind::Docker),
    // 6. Generic runtimes: process name starts with
    name_starts(6, "node", Kind::Node),
    name_starts(6, "bun", Kind::Bun),
    name_starts(6, "deno", Kind::Deno),
    name_starts(6, "python", Kind::Python),
    name_starts(6, "ruby", Kind::Ruby),
    name_starts(6, "php", Kind::Php),
    name_starts(6, "java", Kind::Java),
    name_starts(6, "dotnet", Kind::Dotnet),
    // go: exact names only, so "Google Chrome" never matches a "go" prefix.
    name_is(6, "go", Kind::Go),
    name_is(6, "air", Kind::Go),
    name_starts(6, "go-build", Kind::Go),
];

/// Classifies a process by its name and joined argv.
pub fn classify(name: &str, cmd: &str) -> Kind {
    matching_rule(name, cmd).map_or(Kind::Other, |i| RULES[i].kind)
}

/// Index into [`RULES`] of the first rule that matches, if any.
fn matching_rule(name: &str, cmd: &str) -> Option<usize> {
    let name = normalize_name(name);
    let tokens = tokenize(cmd);
    RULES.iter().position(|rule| match rule.pattern {
        Match::ArgvTokens(needle) => {
            argv_matches(&tokens, needle)
                || (PACKAGE_GROUPS.contains(&rule.group) && package_matches(&tokens, needle))
        }
        Match::NameEquals(want) => name == want,
        Match::NameStartsWith(prefix) => name.starts_with(prefix),
    })
}

/// Rule groups whose argv needles also match an npm package path
/// (`node_modules/<pkg>/`): 1 = frontend frameworks, 3 = tools.
const PACKAGE_GROUPS: &[u8] = &[1, 3];

/// Extensions removed from a token's basename before comparing (one at most).
const STRIPPED_EXTENSIONS: &[&str] = &["js", "cjs", "mjs", "ts", "py", "exe", "cmd", "bat", "ps1"];

/// One whitespace-separated piece of the command line.
#[derive(Debug, PartialEq, Eq)]
struct Token {
    /// Lowercased text after the last `/` or `\`.
    base: String,
    /// `base` with one extension from [`STRIPPED_EXTENSIONS`] removed.
    stem: String,
    /// Lowercased npm packages this token's path runs through: the segment
    /// after each `node_modules/` (`@scope/pkg` for scoped packages).
    packages: Vec<String>,
}

/// Splits a command line into tokens (DESIGN.md section 8, "argv tokens").
fn tokenize(cmd: &str) -> Vec<Token> {
    cmd.split_whitespace()
        .map(|raw| {
            // rsplit always yields at least one item; the fallback is unreachable.
            let base = raw.rsplit(['/', '\\']).next().unwrap_or(raw).to_lowercase();
            let stem = match base.rsplit_once('.') {
                Some((stem, ext)) if STRIPPED_EXTENSIONS.contains(&ext) => stem.to_string(),
                _ => base.clone(),
            };
            Token {
                base,
                stem,
                packages: node_modules_packages(raw),
            }
        })
        .collect()
}

/// Packages named by `node_modules/<pkg>/` segments in a path (either
/// separator). The package must be followed by another separator, so a bare
/// `node_modules/vite` (no trailing segment) is not a package path.
/// Scoped packages (`node_modules/@scope/pkg/`) come back as `@scope/pkg`.
fn node_modules_packages(raw: &str) -> Vec<String> {
    let lower = raw.to_lowercase();
    let segs: Vec<&str> = lower.split(['/', '\\']).collect();
    let mut out = Vec::new();
    for (i, seg) in segs.iter().enumerate() {
        if *seg != "node_modules" {
            continue;
        }
        match segs.get(i + 1) {
            Some(scope) if scope.starts_with('@') => {
                if let (Some(name), Some(_)) = (segs.get(i + 2), segs.get(i + 3))
                    && !name.is_empty()
                {
                    out.push(format!("{scope}/{name}"));
                }
            }
            Some(pkg) if !pkg.is_empty() && segs.get(i + 2).is_some() => {
                out.push((*pkg).to_string());
            }
            _ => {}
        }
    }
    out
}

/// True when some token's path runs through `node_modules/<pkg>/` and
/// `<pkg>` equals the needle's package name: its first word (`next dev` is
/// the `next` package). Scoped needles (`@scope/pkg`) must match exactly.
fn package_matches(tokens: &[Token], needle: &str) -> bool {
    let Some(pkg) = needle.split_whitespace().next() else {
        return false;
    };
    tokens.iter().any(|t| t.packages.iter().any(|p| p == pkg))
}

/// True when the needle's words match that many consecutive tokens.
fn argv_matches(tokens: &[Token], needle: &str) -> bool {
    let words: Vec<&str> = needle.split_whitespace().collect();
    if words.is_empty() {
        return false;
    }
    tokens
        .windows(words.len())
        .any(|run| run.iter().zip(&words).all(|(t, w)| word_matches(t, w)))
}

/// One needle word against one token.
///
/// A word containing `.` (`http.server`, `manage.py`) must equal the whole
/// basename. Any other word must equal the extension-stripped basename, or
/// be followed there by `-` (`jupyter` matches `jupyter-lab`, `next` does not
/// match `nextcloud`).
fn word_matches(token: &Token, word: &str) -> bool {
    if word.contains('.') {
        return token.base == word;
    }
    token.stem == word
        || token
            .stem
            .strip_prefix(word)
            .is_some_and(|rest| rest.starts_with('-'))
}

/// Lowercases the name and strips a trailing `.exe` (Windows executables).
///
/// The strip is applied on every OS: a `.exe` suffix only occurs in practice
/// on Windows, and doing it unconditionally keeps this function testable on
/// all CI runners.
fn normalize_name(name: &str) -> String {
    let lower = name.trim().to_lowercase();
    match lower.strip_suffix(".exe") {
        Some(stem) => stem.to_string(),
        None => lower,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Generates one #[test] per case and a CASES table for the coverage test.
    macro_rules! rule_cases {
        ($($test:ident: ($name:expr, $cmd:expr) => $kind:expr;)*) => {
            $(
                #[test]
                fn $test() {
                    assert_eq!(classify($name, $cmd), $kind);
                }
            )*
            const CASES: &[(&str, &str, Kind)] = &[$(($name, $cmd, $kind)),*];
        };
    }

    rule_cases! {
        // 1. Frontend frameworks
        rule_vite: ("node", "node /home/me/web/node_modules/.bin/vite --port 5173") => Kind::Vite;
        rule_next_dev: ("node", "node /home/me/shop/node_modules/.bin/next dev") => Kind::NextJs;
        rule_next_server: ("next-server (v14.2.3)", "next-server (v14.2.3)") => Kind::NextJs;
        rule_webpack: ("node", "node node_modules/.bin/webpack serve --mode development") => Kind::Webpack;
        rule_react_scripts: ("node", "node /home/me/app/node_modules/.bin/react-scripts start") => Kind::ReactScripts;
        rule_astro: ("node", "node /home/me/blog/node_modules/.bin/astro dev") => Kind::Astro;
        rule_nuxt: ("node", "node /home/me/site/node_modules/nuxt/bin/nuxt.mjs dev") => Kind::Nuxt;
        rule_expo: ("node", "node /home/me/mobile/node_modules/.bin/expo start") => Kind::Expo;
        rule_metro: ("node", "node /home/me/rn/node_modules/.bin/metro serve --port 8081") => Kind::Metro;
        // 2. Backend frameworks
        rule_django: ("python3", "python3 manage.py runserver 0.0.0.0:8000") => Kind::Django;
        rule_uvicorn: ("python3", "/home/me/api/.venv/bin/python3 /home/me/api/.venv/bin/uvicorn main:app --reload") => Kind::FastApi;
        rule_flask: ("python3", "/usr/bin/python3 -m flask --app hello run --debug") => Kind::Flask;
        rule_gunicorn: ("python3", "/srv/app/.venv/bin/python3 /srv/app/.venv/bin/gunicorn -w 4 app:app") => Kind::Gunicorn;
        rule_rails: ("ruby", "ruby bin/rails server -p 3000") => Kind::Rails;
        rule_puma: ("ruby", "puma 6.4.2 (tcp://localhost:9292) [shop]") => Kind::Puma;
        rule_laravel: ("php", "php artisan serve --port=8000") => Kind::Laravel;
        rule_spring: ("java", "/usr/bin/java -classpath /opt/maven/boot/plexus-classworlds-2.8.0.jar org.codehaus.plexus.classworlds.launcher.Launcher spring-boot:run") => Kind::Spring;
        // 3. Tools
        rule_jupyter: ("python3.12", "/opt/homebrew/bin/python3.12 /opt/homebrew/bin/jupyter-lab --no-browser") => Kind::Jupyter;
        rule_streamlit: ("python3", "/home/me/dash/.venv/bin/python3 /home/me/dash/.venv/bin/streamlit run app.py") => Kind::Streamlit;
        rule_http_server: ("python3", "python3 -m http.server 8000 --bind 0.0.0.0") => Kind::PythonHttp;
        rule_ollama: ("ollama", "/usr/local/bin/ollama serve") => Kind::Ollama;
        // 4. Databases / infra
        rule_postgres: ("postgres", "/opt/homebrew/opt/postgresql@16/bin/postgres -D /opt/homebrew/var/postgresql@16") => Kind::Postgres;
        rule_mysqld: ("mysqld", "/usr/sbin/mysqld --daemonize --pid-file=/run/mysqld/mysqld.pid") => Kind::Mysql;
        rule_mariadbd: ("mariadbd", "/usr/sbin/mariadbd") => Kind::Mariadb;
        rule_redis: ("redis-server", "/opt/homebrew/opt/redis/bin/redis-server 127.0.0.1:6379") => Kind::Redis;
        rule_mongod: ("mongod", "/usr/bin/mongod --config /etc/mongod.conf") => Kind::Mongodb;
        rule_nginx: ("nginx", "nginx: master process /usr/sbin/nginx -g daemon on; master_process on;") => Kind::Nginx;
        rule_caddy: ("caddy", "caddy run --config /etc/caddy/Caddyfile") => Kind::Caddy;
        // 5. Container plumbing
        rule_docker_proxy: ("docker-proxy", "/usr/bin/docker-proxy -proto tcp -host-ip 0.0.0.0 -host-port 5432 -container-ip 172.17.0.2 -container-port 5432") => Kind::Docker;
        rule_com_docker_backend: ("com.docker.backend", "/Applications/Docker.app/Contents/MacOS/com.docker.backend") => Kind::Docker;
        rule_vpnkit: ("vpnkit", "/Applications/Docker.app/Contents/Resources/bin/vpnkit forward") => Kind::Docker;
        rule_wslrelay: ("wslrelay.exe", "C:\\Program Files\\WSL\\wslrelay.exe --mode 1") => Kind::Docker;
        // 6. Generic runtimes
        rule_node: ("node", "node server.js") => Kind::Node;
        rule_bun: ("bun", "bun run src/index.ts") => Kind::Bun;
        rule_deno: ("deno", "deno run --allow-net main.ts") => Kind::Deno;
        rule_python: ("Python", "/Library/Frameworks/Python.framework/Versions/3.12/bin/python3 app.py") => Kind::Python;
        rule_ruby: ("ruby", "ruby app.rb") => Kind::Ruby;
        rule_php: ("php", "php -S localhost:8080 -t public") => Kind::Php;
        rule_java: ("java", "/usr/bin/java -jar target/demo-0.0.1-SNAPSHOT.jar") => Kind::Java;
        rule_dotnet: ("dotnet", "dotnet bin/Debug/net8.0/Api.dll") => Kind::Dotnet;
        rule_go: ("go", "go run ./cmd/api") => Kind::Go;
        rule_air: ("air", "air -c .air.toml") => Kind::Go;
        rule_go_build: ("go-build3141592", "/tmp/go-build3141592/b001/exe/main") => Kind::Go;
        // 7. Fallback
        fallback_other: ("sshd", "sshd: /usr/sbin/sshd -D [listener] 0 of 10-100 startups") => Kind::Other;
    }

    #[test]
    fn every_rule_is_the_first_match_for_some_case() {
        for (i, rule) in RULES.iter().enumerate() {
            let covered = CASES
                .iter()
                .any(|(name, cmd, _)| matching_rule(name, cmd) == Some(i));
            assert!(covered, "rule {i} ({rule:?}) has no test case");
        }
    }

    #[test]
    fn rules_are_ordered_by_group() {
        assert!(
            RULES.windows(2).all(|w| w[0].group <= w[1].group),
            "RULES must stay in spec priority order"
        );
    }

    #[test]
    fn patterns_are_lowercase() {
        for rule in RULES {
            let (Match::ArgvTokens(p) | Match::NameEquals(p) | Match::NameStartsWith(p)) =
                rule.pattern;
            assert_eq!(p, p.to_lowercase(), "{rule:?}");
        }
    }

    // Ordering: a framework must beat the runtime it runs on.

    #[test]
    fn vite_beats_node() {
        assert_eq!(
            classify("node", "node /app/node_modules/.bin/vite"),
            Kind::Vite
        );
    }

    #[test]
    fn next_beats_node() {
        assert_eq!(
            classify("node", "node /app/node_modules/.bin/next dev -p 3000"),
            Kind::NextJs
        );
    }

    #[test]
    fn uvicorn_beats_python() {
        assert_eq!(
            classify(
                "python3",
                "python3 -m uvicorn main:app --reload --host 0.0.0.0"
            ),
            Kind::FastApi
        );
    }

    #[test]
    fn django_beats_python() {
        assert_eq!(
            classify("python", "python manage.py runserver"),
            Kind::Django
        );
    }

    #[test]
    fn docker_proxy_is_docker_not_other() {
        assert_eq!(
            classify(
                "docker-proxy",
                "/usr/bin/docker-proxy -proto tcp -host-port 6379"
            ),
            Kind::Docker
        );
    }

    #[test]
    fn rails_beats_ruby() {
        assert_eq!(classify("ruby", "ruby bin/rails s"), Kind::Rails);
    }

    // Matching details.

    #[test]
    fn matching_is_case_insensitive() {
        assert_eq!(
            classify("NODE", "NODE ./node_modules/.bin/VITE"),
            Kind::Vite
        );
        assert_eq!(classify("Postgres", ""), Kind::Postgres);
    }

    #[test]
    fn exe_suffix_is_stripped_from_names() {
        assert_eq!(
            classify("node.exe", "C:\\Program Files\\nodejs\\node.exe server.js"),
            Kind::Node
        );
        assert_eq!(classify("com.docker.backend.EXE", ""), Kind::Docker);
        assert_eq!(classify("postgres.exe", ""), Kind::Postgres);
    }

    #[test]
    fn name_rules_do_not_match_argv() {
        // A db tool talking about postgres is not postgres.
        assert_eq!(
            classify("pgcli", "pgcli postgres://localhost/app"),
            Kind::Other
        );
    }

    #[test]
    fn go_rule_does_not_match_google_chrome() {
        assert_eq!(
            classify(
                "Google Chrome Helper",
                "Google Chrome Helper --type=utility"
            ),
            Kind::Other
        );
    }

    #[test]
    fn empty_input_is_other() {
        assert_eq!(classify("", ""), Kind::Other);
    }

    // Token matching: substrings inside paths or longer words must not match.

    macro_rules! classify_cases {
        ($($test:ident: ($name:expr, $cmd:expr) => $kind:expr;)*) => {
            $(
                #[test]
                fn $test() {
                    assert_eq!(classify($name, $cmd), $kind);
                }
            )*
        };
    }

    classify_cases! {
        // Regressions: the framework name only appears inside another word.
        no_vite_in_invite_app: ("node", "node ~/code/invite-app/server.js") => Kind::Node;
        no_astro_in_astronomy_site: ("node", "node /home/u/astronomy-site/index.js") => Kind::Node;
        no_expo_in_export: ("node", "node scripts/export.js") => Kind::Node;
        no_metro_in_metronome_app: ("node", "node /srv/metronome-app/main.js") => Kind::Node;
        no_rails_in_guardrails: ("python3", "python3 /home/u/guardrails/app.py") => Kind::Python;
        no_spring_in_springboard: ("java", "java -jar springboard.jar") => Kind::Java;
        no_next_in_nextcloud: ("node", "node nextcloud-sync.js") => Kind::Node;
        // Positives, including Windows paths and npm .cmd shims.
        vite_bin_shim: ("node", "node /app/node_modules/.bin/vite --port 5173") => Kind::Vite;
        vite_windows_js: ("node.exe", "node C:\\app\\node_modules\\vite\\bin\\vite.js") => Kind::Vite;
        vite_windows_cmd_shim: ("cmd.exe", "C:\\app\\node_modules\\.bin\\vite.cmd") => Kind::Vite;
        vite_mjs: ("node", "node /app/node_modules/vite/bin/vite.mjs") => Kind::Vite;
        next_dev_full_path: ("node", "node /app/node_modules/next/dist/bin/next dev") => Kind::NextJs;
        next_server_title: ("next-server (v15.0.3)", "next-server (v15.0.3)") => Kind::NextJs;
        uvicorn_module: ("python", "python -m uvicorn main:app --reload") => Kind::FastApi;
        django_plain: ("python", "python manage.py runserver") => Kind::Django;
        django_dot_slash: ("python3", "python3 ./manage.py runserver 8001") => Kind::Django;
        http_server_module: ("python3", "python3 -m http.server 8000") => Kind::PythonHttp;
        jupyter_lab: ("jupyter-lab", "jupyter-lab --no-browser") => Kind::Jupyter;
        // Multi-word needles need consecutive tokens.
        next_and_dev_apart: ("node", "node next build --dev") => Kind::Node;
        manage_py_other_command: ("python3", "python3 manage.py migrate runserver-notes") => Kind::Python;
        artisan_serve: ("php", "php artisan serve") => Kind::Laravel;
        artisan_other_command: ("php", "php artisan queue:work") => Kind::Php;
        // Dotted needles compare the whole basename, extension included.
        manage_py_needs_py: ("python3", "python3 manage runserver") => Kind::Python;
        http_server_needs_exact_token: ("python3", "python3 -m http.server2") => Kind::Python;
        // node_modules package rule: node_modules/<pkg>/ names a frontend/tool rule.
        pkg_react_scripts_start: ("node", "node /app/node_modules/react-scripts/scripts/start.js") => Kind::ReactScripts;
        pkg_vite_windows: ("node.exe", "node C:\\app\\node_modules\\vite\\bin\\vite.js") => Kind::Vite;
        pkg_next_start_server: ("node", "node /app/node_modules/next/dist/server/lib/start-server.js") => Kind::NextJs;
        pkg_case_insensitive: ("node", "node C:\\App\\Node_Modules\\Astro\\dist\\cli\\index.js") => Kind::Astro;
        pkg_nested: ("node", "node /app/node_modules/foo/node_modules/nuxt/bin/x.js") => Kind::Nuxt;
        pkg_expo_cli: ("node", "node /m/node_modules/expo/bin/cli") => Kind::Expo;
        pkg_metro_dir: ("node", "node /rn/node_modules/metro/src/cli.js") => Kind::Metro;
        pkg_unknown_package: ("node", "node /code/invite-app/node_modules/express/index.js") => Kind::Node;
        pkg_name_in_project_dir: ("node", "node /code/my-vite-notes/server.js") => Kind::Node;
        pkg_needs_exact_package: ("node", "node /app/node_modules/vite-plugin-foo/index.js") => Kind::Node;
        pkg_scoped_never_matches_unscoped: ("node", "node /app/node_modules/@vitejs/vite/bin/x.js") => Kind::Node;
        pkg_backend_group_excluded: ("node", "node /app/node_modules/flask/x.js") => Kind::Node;
        java_jar_stays_java: ("java", "java -jar app.jar") => Kind::Java;
    }

    #[test]
    fn tokenize_takes_basename_lowercases_and_strips_one_extension() {
        let t = tokenize("NODE C:\\App\\Vite.JS /x/y/app.d.ts manage.py");
        let stems: Vec<&str> = t.iter().map(|t| t.stem.as_str()).collect();
        let bases: Vec<&str> = t.iter().map(|t| t.base.as_str()).collect();
        assert_eq!(bases, ["node", "vite.js", "app.d.ts", "manage.py"]);
        assert_eq!(stems, ["node", "vite", "app.d", "manage"]);
    }

    #[test]
    fn unlisted_extensions_are_kept() {
        let t = tokenize("springboard.jar");
        assert_eq!(t[0].stem, "springboard.jar");
    }

    #[test]
    fn node_modules_packages_are_extracted() {
        assert_eq!(
            node_modules_packages("/a/node_modules/vite/bin/vite.js"),
            ["vite"]
        );
        assert_eq!(
            node_modules_packages("C:\\a\\NODE_MODULES\\@Scope\\Pkg\\x.js"),
            ["@scope/pkg"]
        );
        assert_eq!(
            node_modules_packages("/a/node_modules/x/node_modules/y/z"),
            ["x", "y"]
        );
        // Scoped package needs a trailing segment after the name too.
        assert!(node_modules_packages("/a/node_modules/@scope/pkg").is_empty());
        assert!(node_modules_packages("/a/node_modules//x").is_empty());
        assert!(node_modules_packages("node_modules").is_empty());
    }

    #[test]
    fn scoped_needle_matches_only_exactly() {
        let tokens = tokenize("node /a/node_modules/@scope/pkg/bin.js");
        assert!(package_matches(&tokens, "@scope/pkg"));
        assert!(!package_matches(&tokens, "pkg"));
        assert!(!package_matches(&tokens, "@scope"));
    }

    #[test]
    fn empty_needle_never_matches() {
        assert!(!argv_matches(&tokenize("node vite"), ""));
        assert!(!argv_matches(&tokenize(""), "vite"));
    }
}
