<!-- skrrt:ship -->
## Git workflow — skrrt skills

Use the installed skrrt skills for all git shipping operations:

- **Commits**: Use `/commit` to stage changes and write conventional commits. Keep commit messages plain text — no emojis or gitmojis.
- **Pull requests**: Use `/pr` to push branches and open PRs or MRs with the matching forge CLI.
- **Releases**: Use `/release` to draft release notes and publish releases.

Do not write raw `git commit`, `gh pr create`, `gh release create`, `glab mr create`, or
`glab release create` commands manually when these skills are available.

### Deployment conventions (Skrrt)

These rules apply regardless of branching strategy:

- **Tag format:** `vX.Y.Z` (production), `vX.Y.Z-rc.N` (release candidate), `vX.Y.Z-{env}.N` (custom tier). Always use annotated tags.
- **Tags are immutable.** Never delete or move a tag. If a release is bad, cut a new patch version.
- **Build once, promote the same artifact.** The artifact tested in staging must be identical to what reaches production. Never rebuild from a tag.
- **Lower environments do not need tags.** Dev deploys from branch HEAD on merge. Preview environments are per-PR and SHA-scoped.
- **Manual `workflow_dispatch`** can promote an existing artifact to any environment. It complements the tag-driven flow, not replaces it.

<!-- skrrt:branching -->
## Branching strategy — GitHub Flow

This project uses **GitHub Flow**. All agents and contributors must follow these rules:

### Branch rules

- `develop` is the only long-lived branch and is always deployable.
- All work happens on short-lived, descriptively named branches.
- Never commit directly to `develop` — all changes reach `develop` through a pull request.
- PRs always target `develop`.
- Feature branches must be up to date with `develop` before merging.
- Feature branches are deleted after merge.
- CI runs on every PR.
- Releases are cut by tagging commits on `develop`.
- Do not create `release/*` or `hotfix/*` branches.

### Branch naming

Use `<type>/<short-description>` with lowercase and hyphens:
- Features: `feat/add-auth`, `feat/search-index`
- Fixes: `fix/login-redirect`, `fix/null-check`
- Other: `docs/api-guide`, `chore/update-deps`, `refactor/auth-module`

### Keeping branches up to date (Skrrt convention)

- Before opening a PR, rebase the feature branch onto `develop`: `git pull --rebase origin develop`
- If the rebase has conflicts, resolve them and run `git rebase --continue`.
- If the rebase cannot be resolved cleanly, abort with `git rebase --abort` and ask the user for help.

### PR merge strategy (Skrrt convention)

- Use a **merge commit** — the feature branch's commits are preserved on `develop`.
- One PR = one logical change; the branch's individual commits stay visible in history.

### Tagging and environment (Skrrt convention)

Tags are placed **on `develop` only** — never on feature branches. See shared deployment conventions above.

| Environment | Trigger | Tag? |
| --- | --- | --- |
| Dev | Merge to `develop` (merge commit) | No |
| Staging | Tag `vX.Y.Z-rc.N` on `develop` | Yes |
| Production | Tag `vX.Y.Z` on `develop` | Yes |

- Promote to staging by tagging an RC on `develop`. If it fails, merge fixes via PR and tag a new RC.
- Promote to production by tagging a clean semver release on the validated commit.

### Agent lifecycle (full auto)

1. Create a branch from `develop`: `git switch -c <type>/<description>`
2. Make changes and commit using `/commit`.
3. Before opening a PR, rebase onto `develop`: `git pull --rebase origin develop`
4. Push and open a PR using `/pr` — target is always `develop`.
5. After merge, the branch is deleted automatically by the forge.
6. To promote to staging, tag an RC on `develop`: use `/release` with a pre-release tag.
7. After staging validation, tag the production release on `develop`: use `/release`.
<!-- /skrrt:branching -->
