<!--
Thanks for contributing to VoidLauncher!

Before opening this PR:
- read CONTRIBUTING.md;
- keep the change focused — unrelated work belongs in a separate PR;
- never include access tokens, passwords, private keys or personal data.
-->

## Summary

<!-- What does this PR change, and why? Keep it short. -->

## Related issue

<!-- Link the issue this PR addresses, for example: Closes #123. -->

## Type of change

- [ ] Bug fix
- [ ] New feature
- [ ] Performance improvement
- [ ] Refactor / internal cleanup
- [ ] Documentation
- [ ] CI / build / tooling

## What changed

<!--
Bullet points. Mention user-visible behavior and the main modules/areas affected.
If this is a UI change, mention whether both English and Russian strings were updated.
-->

## Why

<!-- The problem this solves. Link logs, issues or screenshots when useful. -->

## How tested

<!-- Exact commands and manual scenarios. Do not just write "tests pass". -->

Commands run:

- [ ] `npx tsc --noEmit`
- [ ] `npm test`
- [ ] `npm run build`
- [ ] `cd src-tauri && cargo fmt --all -- --check`
- [ ] `cd src-tauri && cargo check`
- [ ] `cd src-tauri && cargo clippy --all-targets -- -D warnings`
- [ ] `cd src-tauri && cargo test`

Manual testing:

<!--
Windows version, Minecraft version and loader, steps performed, and the result.
For launcher/window/update changes, say which build you used.
-->

## UI changes

<!--
Screenshots or a short recording for visible changes.
Confirm that user-facing text exists in both `src/lib/i18n/en.ts` and `ru.ts`.
Write "None" if the PR does not change the UI.
-->

## Checklist

- [ ] I read `CONTRIBUTING.md`.
- [ ] The PR is focused and does not include unrelated commits.
- [ ] I added or updated tests for behavior changes.
- [ ] I updated documentation or comments where needed.
- [ ] No tokens, passwords, private keys or personal data are included.
- [ ] I did not bump the project version; maintainers handle releases.
- [ ] I tested on Windows 10/11 if the change touches the desktop app.
- [ ] If AI-assisted, I reviewed and understand every changed line.
