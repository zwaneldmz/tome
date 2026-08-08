# Policy presets: the allowlist is data

The air-gap allowlist is not code and not a setting buried in a dialog — it's a JSON file a repo can commit:

```json
{ "allow": ["api.anthropic.com", "claude.ai"] }
```

Place it at `.tome/airgap.json`. When a workspace opens, Tome validates every pattern, shows the user exactly which hosts the repo is asking to reach, and honors the file only after explicit consent. Consent is per-user, per-repo, and pinned to the file's SHA-1 — edit one character and every consent for it is dropped. The full validation and consent story, including why a committed file is treated as untrusted input, is in [THREATMODEL.md](THREATMODEL.md).

## Starter presets

Copy one into `.tome/airgap.json` and commit it.

**anthropic-only** — the smallest useful set:

```json
{
  "allow": [
    "api.anthropic.com",
    "claude.ai",
    "console.anthropic.com",
    "statsig.anthropic.com"
  ]
}
```

**multi-provider** — the full set Tome ships as defaults:

```json
{
  "allow": [
    "api.anthropic.com",
    "claude.ai",
    "console.anthropic.com",
    "statsig.anthropic.com",
    "api.openai.com",
    "auth.openai.com",
    "generativelanguage.googleapis.com",
    "oauth2.googleapis.com",
    "openrouter.ai",
    "router.requesty.ai",
    "api.deepseek.com",
    "api.moonshot.ai",
    "api.groq.com",
    "api.mistral.ai",
    "api.x.ai",
    "bedrock-runtime.*.amazonaws.com"
  ]
}
```

**bedrock** — nothing to add: `bedrock-runtime.*.amazonaws.com` is already in the defaults above. It's also the reason interior wildcards exist: `*` matches exactly one DNS label, so the pattern covers `bedrock-runtime.us-east-1.amazonaws.com` and can never match `amazonaws.com.evil.com`. If you commit a repo allowlist, you can trim it to just the regions you use:

```json
{
  "allow": [
    "api.anthropic.com",
    "bedrock-runtime.us-east-1.amazonaws.com"
  ]
}
```

## Distributing a policy in an org

Commit the file per-repo — that's the whole mechanism. An org that wants a canonical policy publishes one `.tome/airgap.json` (a repo, a gist, an internal page) and teams copy it into their projects.

Consent is per-user and pinned to the content hash, so when the org updates the canonical file, every user is re-prompted once on next open. That re-prompt is a feature, not friction: the user *sees* the change — the diff in reachable hosts is exactly the thing an attacker editing the file would want to hide. Silent policy updates would defeat the point of consent.

## Format rules

Validation refuses a pattern (and tells the user why) when it:

- **isn't at least two labels** (`localhost`) — single-label names can't be reasoned about; `api.example.com` is the shape.
- **is a bare `*`** — matches every host; that's not an allowlist, it's disabling the air gap.
- **wildcards the TLD** (`*.com`, `*.*`) — matches whole slices of the internet.
- **leads with `*` on a two-label name** (`*.co` is refused; `*.example.com` is fine) — a wildcard base domain is the same breadth class as a wildcard TLD.
- **uses `*` as a partial label** (`*api.example.com`) — `*api` would also match `evilapi`; wildcards are whole labels only.
- **contains URL syntax** (`https://`, `/`, `@`) — patterns are bare hostnames; a scheme or path is an attempt to talk around the anchored matcher.

## Out of scope

Hosted preset distribution — browsing and subscribing to presets from a registry — is a future story, not built. Today distribution is: copy a file, commit it, consent.
