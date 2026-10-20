//! Workflow prompt templates, surfaced as MCP prompts and REST GET /prompts/{name}.
//!
//! A prompt is a multi-step recipe rendered as markdown: the agent reads it and
//! follows it. Each template therefore names the exact tool to call at every
//! step, in order, with a realistic JSON argument example taken from that
//! tool's real schema in tools.rs. Nothing here invents a tool or a parameter:
//! a prompt that told an agent to call a nonexistent tool would fail at the
//! first step.
//!
//! Arguments are optional strings. A missing argument is not an error: the
//! template falls back to a discovery step, which is more useful than refusing
//! to render.

use std::collections::HashMap;

use super::scope::Scope;
use super::{PromptArgument, PromptSpec};

pub static PROMPTS: &[PromptSpec] = &[
    PromptSpec {
        name: "add-messenger",
        description: "Add a well-known messenger or web app to bzdium: look up a built-in recipe, validate with a dry run, create the service, focus it and verify it loaded.",
        scope: Scope::Write,
        arguments: args_add_messenger,
        render: render_add_messenger,
    },
    PromptSpec {
        name: "triage-unread",
        description: "Find which services have unread messages, rank them by unread count, and focus the busiest one.",
        scope: Scope::Write,
        arguments: args_none,
        render: render_triage_unread,
    },
    PromptSpec {
        name: "workspace-backup",
        description: "Export the bzdium workspace (services and settings) to a JSON file at a path you choose, then verify the file exists and holds what was expected.",
        scope: Scope::Admin,
        arguments: args_workspace_backup,
        render: render_workspace_backup,
    },
    PromptSpec {
        name: "tls-bypass-audit",
        description: "Audit which services trust invalid TLS certificates, explain the risk of each one and confirm whether the bypass is even permitted for that host.",
        scope: Scope::Read,
        arguments: args_none,
        render: render_tls_bypass_audit,
    },
    PromptSpec {
        name: "recover-hung-service",
        description: "Diagnose a wedged service page and recover it with a hard reload, then verify the page is responsive again.",
        scope: Scope::Write,
        arguments: args_recover_hung,
        render: render_recover_hung_service,
    },
];

/// Exact-name lookup. Unknown names are turned into UNKNOWN_PROMPT by the
/// transport, which owns the client-facing wording.
pub fn find_prompt(name: &str) -> Option<&'static PromptSpec> {
    PROMPTS.iter().find(|p| p.name == name)
}

/// The MCP prompts/list and REST /prompts payload.
pub fn prompts_list() -> serde_json::Value {
    serde_json::Value::Array(
        PROMPTS
            .iter()
            .map(|p| {
                serde_json::json!({
                    "name": p.name,
                    "description": p.description,
                    "scope": p.scope.as_str(),
                    "arguments": (p.arguments)(),
                })
            })
            .collect(),
    )
}

// ---------------------------------------------------------------------------
// Argument declarations
// ---------------------------------------------------------------------------

fn args_none() -> Vec<PromptArgument> {
    Vec::new()
}

fn args_add_messenger() -> Vec<PromptArgument> {
    vec![PromptArgument {
        name: "service",
        description: "Name of the messenger to add, matched case-insensitively against the built-in recipes, e.g. \"Signal\" or \"Google Chat\". Omit to have the agent list the catalog and ask.",
        required: false,
    }]
}

fn args_workspace_backup() -> Vec<PromptArgument> {
    vec![PromptArgument {
        name: "path",
        description: "Absolute path of the JSON file to write, e.g. C:\\Users\\me\\bzdium-backup.json. Parent directories must already exist.",
        required: true,
    }]
}

fn args_recover_hung() -> Vec<PromptArgument> {
    vec![PromptArgument {
        name: "serviceId",
        description: "Id of the wedged service. Omit to have the agent read the service list and choose the one that is active or not responding.",
        required: false,
    }]
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Reads an argument, treating blank and whitespace-only values as absent so a
/// client that sends an empty string still gets the useful discovery path.
fn arg<'a>(args: &'a HashMap<String, String>, key: &str) -> Option<&'a str> {
    args.get(key).map(|v| v.trim()).filter(|v| !v.is_empty())
}

/// Renders the specifics the agent looks for, or a discovery instruction when
/// the caller left the argument out.
fn target_or_discovery(args: &HashMap<String, String>, key: &str, discovery: &str) -> String {
    match arg(args, key) {
        Some(v) => format!("**Target: {v}** (supplied as the {key} argument.)"),
        None => {
            format!("No {key} was supplied. Start with the discovery step below.\n\n{discovery}")
        }
    }
}

// ---------------------------------------------------------------------------
// Renderers
// ---------------------------------------------------------------------------

fn render_add_messenger(args: &HashMap<String, String>) -> String {
    let target = target_or_discovery(
        args,
        "service",
        "Call `list_recipes` with an empty object and read the catalog. If the user has not \
         named a service, reply with the catalog grouped by category and ask which one to add. \
         Do not guess a URL for an unknown service.",
    );
    format!(
        r#"# Add a messenger to bzdium

{target}

## 1. Find the recipe
Call `list_recipes` with an empty object. Each recipe carries `name`, `url`,
`icon` (a 1-2 character lettermark) and `iconId` (a brand icon slug).

```json
{{}}
```

Match the requested name case-insensitively. If nothing matches, stop and ask
the user for the exact https URL instead of inventing one.

## 2. Preview the change
`add_service` is mutating, so preview it first. The dry run runs the very same
validators as the real call: https-only, non-empty name, and the TLS policy.

```json
{{
  "name": "Signal",
  "url": "https://signal.example",
  "icon": "SI",
  "iconId": "signal",
  "dryRun": true
}}
```

Take `url` and `iconId` from the recipe. `icon` and `iconId` are optional;
omit `iconId` to let bzdium auto-detect a brand icon from the URL host.

## 3. Commit
Re-issue the identical arguments with `"dryRun": false` (or drop the flag).

```json
{{
  "name": "Signal",
  "url": "https://signal.example",
  "icon": "SI",
  "iconId": "signal"
}}
```

The response is the new application snapshot. Record the new service `id`.

## 4. Focus it
Call `set_active_service` with that id so the user sees the page immediately.

```json
{{ "id": "<the new service id>" }}
```

The first load needs the user to sign in inside the isolated session. Say so
plainly; do not attempt to automate credentials.

## 5. Verify
Call `get_service` with the id and confirm `name` and `url` match the recipe.

```json
{{ "id": "<the new service id>" }}
```

Report what was added, its id, and that the session stays empty until the user
signs in.

## Failure handling
- `INVALID_URL` - the URL was not https. Use the recipe URL verbatim.
- `TLS_BYPASS_REFUSED` - `ignoreTlsErrors` is never allowed for a built-in
  public service. Drop it; it only applies to self-hosted hosts.
- `UNKNOWN_SERVICE_ID` - re-read `list_services`; the id came from a stale
  snapshot."#
    )
}

fn render_triage_unread(_args: &HashMap<String, String>) -> String {
    r#"# Triage unread messages

## 1. Read the current state
Call `get_state` with an empty object. It returns `services` sorted by
sidebar order, each with `badgeCount`, plus `activeServiceId` and
`totalBadgeCount`.

```json
{}
```

The same data is available as the resource `bzdium://app/state`.

## 2. Rank
Sort services by `badgeCount` descending and drop every service at zero. Break
ties by sidebar `order` ascending. List services with `enabled: false`
separately rather than ranking them: their badge count is stale because their
page is not running.

If `totalBadgeCount` is 0, say there is nothing unread and stop.

## 3. Report before acting
Present a short table: service name, `badgeCount`, and whether the webview is
`hibernated`. State which one you are about to focus and why.

## 4. Focus the busiest service
Call `set_active_service` with the top-ranked id. This creates or wakes the
webview as needed.

```json
{ "id": "<top-ranked service id>" }
```

## 5. Offer the obvious next step
Mention `hibernate_service` for quiet services and `reorder_services` to move
the busy service to the top of the sidebar. Do not call either without
confirmation: both change the user's layout or runtime state.

```json
{ "orderedIds": ["<busy id>", "<other id>", "<another id>"] }
```

## Notes
- Badge counts come from page titles the service itself reports, so a service
  that never loaded reports 0. If a service you expect to be busy shows 0 and is
  hibernated, wake it with `wake_service` before concluding it is quiet.
- Do not reload a service merely to refresh a badge count."#
        .to_string()
}

fn render_workspace_backup(args: &HashMap<String, String>) -> String {
    let target = target_or_discovery(
        args,
        "path",
        "Ask the user for an absolute path before continuing; a backup that lands \
         somewhere unnamed is not a backup.",
    );
    format!(
        r#"# Back up the bzdium workspace

{target}

## 1. Export
Call `export_config` with the absolute path. The document carries services and
settings only: sessions, cookies and credentials are never included.

```json
{{ "path": "C:\\Users\\me\\bzdium-backup.json" }}
```

The response contains `path`, `confirmationToken` and `note`.

## 2. Keep the confirmation token
**Store the returned `confirmationToken`.** `import_config` refuses to run
without it. If the user intends to restore later in this session, show them the
token and say when it expires.

## 3. Verify the file
Confirm the export actually produced a file: re-read `bzdium://services` and
`bzdium://settings` and compare them against what the user expects. Report the
exported path, the service count, and whether a restore is possible (token
present and unexpired).

Do not claim the file is valid merely because the call returned without error:
the endpoint reports the path it wrote.

## 4. Restoring (only if asked)
`import_config` **replaces all services and settings** from the file, and
requires the token from this export.

```json
{{ "path": "C:\\Users\\me\\bzdium-backup.json", "confirmationToken": "<token from step 1>" }}
```

Saved sessions are kept where service ids match. This tool offers no dry run
because it touches the filesystem: confirm with the user first and describe
exactly what will be replaced.

## Failure handling
- `CONFIG_INVALID` - the file is not a bzdium config document. Call
  `export_config` first to obtain a valid example.
- `CONFIG_TOO_LARGE` - the document exceeds 2 MiB. Reduce the service count.
- `CONFIRMATION_REQUIRED` - the token is missing, wrong or expired. Re-run
  `export_config` to mint a fresh one."#
    )
}

fn render_tls_bypass_audit(_args: &HashMap<String, String>) -> String {
    r#"# Audit TLS bypass

## 1. Read the services
Call `list_services` with an empty object, or read `bzdium://services`.

```json
{}
```

Every service carries `ignoreTlsErrors` and `url`. That setting makes bzdium
accept invalid or self-signed certificates for that service's webview.

## 2. Filter
Collect the services where `ignoreTlsErrors` is true. If there are none, say so
plainly and stop: that is the good outcome, not an empty report.

## 3. Judge each one
For each bypassing service, classify the host:
- A host matching a built-in public recipe (WhatsApp, Gmail, Discord, Slack,
  Teams, and so on) is a policy violation. bzdium refuses `ignoreTlsErrors` for
  those hosts, so a matching entry means the service was edited by hand.
- Any other host is a self-hosted instance and may legitimately need the bypass
  for a private certificate authority.

To test one target, use the dry run: `update_service` applies the same TLS
policy check without committing (both `name` and `url` are required).

```json
{
  "id": "<service id>",
  "name": "<current name>",
  "url": "<current url>",
  "ignoreTlsErrors": true,
  "dryRun": true
}
```

A `TLS_BYPASS_REFUSED` error means the host is a built-in public service and
the bypass must be removed.

## 4. Explain the risk
For each finding, state in one or two sentences what the bypass permits (an
attacker on the network path can read and modify that service's traffic,
including session cookies) and the blast radius (that one isolated webview
session, not the whole app).

Never recommend enabling TLS bypass. Recommend removing it for public hosts and
installing the private certificate authority for self-hosted ones.

## 5. Offer the fix
For services that should not bypass TLS, propose `update_service` with
`ignoreTlsErrors: false` plus the unchanged `name` and `url`. Ask before
calling: this rewrites the service and rebuilds its webview.

This audit only needs read access, but `update_service` is a write operation.
If the current token is read-scoped, do not attempt the call: report the
findings and say a write-scope token is required to apply the fix."#
        .to_string()
}

fn render_recover_hung_service(args: &HashMap<String, String>) -> String {
    let target = target_or_discovery(
        args,
        "serviceId",
        "Call `list_services` and pick the service the user describes as stuck; if \
         that is ambiguous, ask rather than guessing.",
    );
    format!(
        r#"# Recover a hung service

{target}

## 1. Diagnose before you act
Call `get_service` with the id, then read the resource `bzdium://sessions` to
see how many bytes its session profile occupies (names and sizes only: profile
contents are never exposed).

```json
{{ "id": "<the service id>" }}
```

Signs that point at a wedged webview rather than a dead network:
- the page stopped updating while the app stayed responsive,
- reloading through the service's own UI does nothing,
- the badge count stopped changing for a service that is normally active.

`hibernated: true` is not a hang. Call `wake_service` for that instead.

## 2. Try the cheap fix first
`navigate` with `home` reloads the configured URL inside the existing
webview. This is gentler and keeps in-page state where the service allows it.

```json
{{ "id": "<the service id>", "action": "home" }}
```

If the page responds, stop here.

## 3. Hard reload
Still stuck? Call `reload_service`. This is a **hard** reload: bzdium closes the
webview and recreates it. Consequences to state up front:
- the page returns to the service's **configured URL, not the deep link** the
  user was on;
- unsent text in the service's composer is lost;
- the isolated session (logins, cookies) is preserved.

```json
{{ "id": "<the service id>" }}
```

## 4. Verify
Re-read `get_service`, then call `set_active_service` to focus the page so the
user can confirm it is alive.

```json
{{ "id": "<the service id>" }}
```

## 5. If it is still wedged
Report that the recovery failed and name the next escalation instead of
looping: check the service `url` is correct, confirm the host is reachable from
the user's network, and report the session size from step 1. A repeatedly
wedging service is worth removing and re-adding with `remove_service` plus
`add_service`, but that deletes the saved login: ask first.

`remove_service` is an admin operation. If the current token is write-scoped
rather than admin, stop at the diagnosis and ask the user to remove and re-add
the service themselves; an admin-scope token is required for that step.

Do not call `reload_service` more than once per diagnosis."#
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::tools::find_tool;
    use std::collections::HashSet;

    fn args(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn empty() -> HashMap<String, String> {
        HashMap::new()
    }

    // -- registry contract -------------------------------------------------

    #[test]
    fn prompt_names_are_unique_and_kebab_case() {
        let mut seen = HashSet::new();
        for p in PROMPTS {
            assert!(seen.insert(p.name), "duplicate prompt name {}", p.name);
            assert!(
                p.name.chars().all(|c| c.is_ascii_lowercase() || c == '-'),
                "{} must be kebab-case",
                p.name
            );
        }
    }

    #[test]
    fn the_five_documented_prompts_exist() {
        let mut names: Vec<&str> = PROMPTS.iter().map(|p| p.name).collect();
        names.sort_unstable();
        assert_eq!(
            names,
            vec![
                "add-messenger",
                "recover-hung-service",
                "tls-bypass-audit",
                "triage-unread",
                "workspace-backup",
            ]
        );
    }

    #[test]
    fn every_prompt_is_described_for_an_llm() {
        for p in PROMPTS {
            assert!(!p.description.is_empty(), "{} has no description", p.name);
            assert!(
                p.description.len() > 40,
                "{} description is too terse: {:?}",
                p.name,
                p.description
            );
        }
    }

    #[test]
    fn find_prompt_is_exact_match() {
        assert_eq!(find_prompt("triage-unread").unwrap().name, "triage-unread");
        assert!(find_prompt("triage_unread").is_none());
        assert!(find_prompt("TRIAGE-UNREAD").is_none());
        assert!(find_prompt("").is_none());
        assert!(find_prompt("add-messenger ").is_none());
    }

    #[test]
    fn prompts_list_serialises_the_whole_table() {
        let list = prompts_list();
        let rows = list.as_array().unwrap();
        assert_eq!(rows.len(), PROMPTS.len());
        for (row, spec) in rows.iter().zip(PROMPTS.iter()) {
            assert_eq!(row["name"], spec.name);
            assert_eq!(row["description"], spec.description);
            assert_eq!(row["scope"], spec.scope.as_str());
            assert!(row["arguments"].is_array());
        }
    }

    #[test]
    fn prompt_scopes_cover_the_tools_they_drive() {
        // A prompt must never be fetchable at a weaker scope than the tools it
        // tells the agent to call, or a read token could pull admin
        // instructions and then fail halfway through.
        for p in PROMPTS {
            let text = (p.render)(&empty());
            let required = backticked(&text)
                .into_iter()
                .filter_map(find_tool)
                .map(|t| t.scope)
                .max();
            if let Some(required) = required {
                if p.scope >= required {
                    continue;
                }
                // A read-scoped prompt may still describe a write as the fix,
                // but it must say a stronger token is needed instead of
                // implying the call will simply work.
                assert!(
                    text.contains(&format!("{required}-scope token")),
                    "{} names a {required}-scoped tool but declares {} without saying a                      {required}-scope token is required",
                    p.name,
                    p.scope
                );
            }
        }
    }

    #[test]
    fn scopes_match_the_documented_matrix() {
        assert_eq!(find_prompt("tls-bypass-audit").unwrap().scope, Scope::Read);
        assert_eq!(find_prompt("triage-unread").unwrap().scope, Scope::Write);
        assert_eq!(find_prompt("add-messenger").unwrap().scope, Scope::Write);
        assert_eq!(
            find_prompt("recover-hung-service").unwrap().scope,
            Scope::Write
        );
        assert_eq!(find_prompt("workspace-backup").unwrap().scope, Scope::Admin);
    }

    // -- rendering ---------------------------------------------------------

    #[test]
    fn every_prompt_renders_markdown_naming_only_real_tools() {
        for p in PROMPTS {
            let text = (p.render)(&empty());
            assert!(
                text.starts_with("# "),
                "{} must start with a markdown heading",
                p.name
            );
            assert!(
                text.len() > 500,
                "{} render is too thin to be a workflow",
                p.name
            );
            assert!(
                text.contains("json"),
                "{} must show example JSON arguments",
                p.name
            );
            assert!(text.contains("## 1."), "{} must be a numbered workflow", p.name);

            for token in backticked(&text) {
                // Tool names are lowercase snake_case identifiers. Uppercase
                // spans such as INVALID_URL are error codes, not tools, so the
                // lowercase requirement keeps them out of this check.
                let tool_shaped = token.contains('_')
                    && !token.contains(' ')
                    && token.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
                if tool_shaped {
                    assert!(
                        find_tool(token).is_some(),
                        "{} references unregistered tool {}",
                        p.name,
                        token
                    );
                }
            }
        }
    }

    #[test]
    fn every_declared_argument_is_mentioned_in_the_render() {
        for p in PROMPTS {
            let text = (p.render)(&empty());
            for a in (p.arguments)() {
                assert!(
                    text.contains(a.name),
                    "{} declares argument {} but the render never mentions it",
                    p.name,
                    a.name
                );
                assert!(
                    !a.description.is_empty(),
                    "{} argument {} has no description",
                    p.name,
                    a.name
                );
            }
        }
    }

    #[test]
    fn argument_names_are_unique_per_prompt() {
        for p in PROMPTS {
            let mut seen = HashSet::new();
            for a in (p.arguments)() {
                assert!(seen.insert(a.name), "{} declares {} twice", p.name, a.name);
            }
        }
    }

    #[test]
    fn only_the_documented_arguments_are_required() {
        let required = |name: &str| -> Vec<&'static str> {
            (find_prompt(name).unwrap().arguments)()
                .into_iter()
                .filter(|a| a.required)
                .map(|a| a.name)
                .collect()
        };
        assert_eq!(required("add-messenger"), Vec::<&str>::new());
        assert_eq!(required("triage-unread"), Vec::<&str>::new());
        assert_eq!(required("workspace-backup"), vec!["path"]);
        assert_eq!(required("tls-bypass-audit"), Vec::<&str>::new());
        assert_eq!(required("recover-hung-service"), Vec::<&str>::new());
    }

    // -- argument substitution ---------------------------------------------

    #[test]
    fn supplied_arguments_are_rendered_verbatim() {
        let text =
            (find_prompt("add-messenger").unwrap().render)(&args(&[("service", "Google Chat")]));
        assert!(text.contains("Google Chat"));
        assert!(text.contains("Target: Google Chat"));
        assert!(!text.contains("No service was supplied"));

        let text = (find_prompt("workspace-backup").unwrap().render)(&args(&[(
            "path",
            "D:\\backups\\bzdium.json",
        )]));
        assert!(text.contains("D:\\backups\\bzdium.json"));
        assert!(!text.contains("No path was supplied"));

        let text = (find_prompt("recover-hung-service").unwrap().render)(&args(&[(
            "serviceId",
            "abc-123",
        )]));
        assert!(text.contains("Target: abc-123"));
    }

    #[test]
    fn missing_or_blank_arguments_fall_back_to_discovery() {
        for blank in ["", "   ", "\t\n"] {
            let text =
                (find_prompt("add-messenger").unwrap().render)(&args(&[("service", blank)]));
            assert!(
                text.contains("No service was supplied"),
                "blank service {blank:?} must fall back to discovery"
            );
            assert!(!text.contains("**Target:"));

            let text =
                (find_prompt("workspace-backup").unwrap().render)(&args(&[("path", blank)]));
            assert!(text.contains("No path was supplied"));
        }
        // An absent key behaves exactly like a blank one.
        let text = (find_prompt("recover-hung-service").unwrap().render)(&empty());
        assert!(text.contains("No serviceId was supplied"));
    }

    #[test]
    fn extra_arguments_are_ignored_without_panicking() {
        let extra = args(&[
            ("service", "X"),
            ("path", "/tmp/x"),
            ("serviceId", "id"),
            ("bogus", "y"),
        ]);
        for p in PROMPTS {
            assert!(!(p.render)(&extra).is_empty(), "{} rendered nothing", p.name);
        }
    }

    // -- content guarantees ------------------------------------------------

    #[test]
    fn workspace_backup_warns_that_restore_needs_the_token() {
        let text =
            (find_prompt("workspace-backup").unwrap().render)(&args(&[("path", "C:\\b.json")]));
        assert!(text.contains("confirmationToken"));
        assert!(text.contains("export_config"));
        assert!(text.contains("import_config"));
        // A restore REPLACES everything; the agent must say so before acting.
        assert!(text.contains("replaces all services and settings"));
    }

    #[test]
    fn recover_prompt_states_the_hard_reload_semantics() {
        let text =
            (find_prompt("recover-hung-service").unwrap().render)(&args(&[("serviceId", "abc")]));
        assert!(text.contains("reload_service"));
        // The surprise an agent must not be caught by: the deep link is lost.
        assert!(text.contains("configured URL, not the deep link"));
        assert!(text.contains("recreates it"));
    }

    #[test]
    fn tls_audit_prompt_never_recommends_enabling_bypass() {
        let text = (find_prompt("tls-bypass-audit").unwrap().render)(&empty());
        assert!(text.contains("ignoreTlsErrors"));
        assert!(text.contains("Never recommend enabling TLS bypass"));
        assert!(text.contains("list_services"));
        assert!(text.contains("TLS_BYPASS_REFUSED"));
    }

    #[test]
    fn triage_prompt_ranks_by_badge_count() {
        let text = (find_prompt("triage-unread").unwrap().render)(&empty());
        assert!(text.contains("badgeCount"));
        assert!(text.contains("get_state"));
        assert!(text.contains("set_active_service"));
        assert!(text.contains("hibernated"));
    }

    #[test]
    fn no_prompt_pretends_session_contents_are_readable() {
        for p in PROMPTS {
            let text = (p.render)(&empty());
            for bad in [
                "read the cookies",
                "profile contents are exposed",
                "dump the session",
            ] {
                assert!(
                    !text.contains(bad),
                    "{} must not imply session contents are readable",
                    p.name
                );
            }
        }
    }

    /// Every backticked span in a rendered prompt.
    fn backticked(text: &str) -> Vec<&str> {
        let mut out = Vec::new();
        let mut rest = text;
        while let Some(start) = rest.find('`') {
            let after = &rest[start + 1..];
            let Some(end) = after.find('`') else { break };
            out.push(&after[..end]);
            rest = &after[end + 1..];
        }
        out
    }
}
