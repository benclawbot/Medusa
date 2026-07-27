# Authenticated Windows end-to-end tests

This directory contains the opt-in test harness for exercising Medusa against a real ChatGPT OAuth session on Windows.

The live suite deliberately runs only on a self-hosted Windows runner configured under the same Windows user account that owns the `openai-oauth` credential. The gateway owns and reads that credential; Medusa only talks to the local OpenAI-compatible endpoint at `127.0.0.1:10531`.

## Runner requirements

- Windows 11
- Git, Rust 1.88, Node.js 22 and npm
- GitHub Actions runner installed for the interactive user, with labels `self-hosted`, `Windows`, `X64`, and `medusa-oauth`
- A completed `openai-oauth` browser login for that user
- Permission to launch desktop applications from the runner session

Do not install this runner as `LocalSystem`: that account cannot reuse the interactive user's OAuth credential or desktop session.

## Bootstrap the runner

1. In GitHub, open **Settings → Actions → Runners → New self-hosted runner** for this repository and copy the short-lived registration token.
2. Open PowerShell as the same interactive Windows user that completed ChatGPT OAuth.
3. From a Medusa checkout, run:

```powershell
$token = Read-Host "Runner registration token"
pwsh -File scripts/e2e/bootstrap-windows-oauth-runner.ps1 `
  -RunnerToken $token `
  -Start
```

The bootstrap verifies Windows, Git, Node.js 22, Rust 1.88, and the local OAuth gateway; downloads and configures the GitHub Actions runner; applies the `medusa-oauth` label; and starts it in the current interactive desktop session. It deliberately does not install the runner as a Windows service.

The registration token is passed only to GitHub's `config.cmd`. Do not put it in source control, shell history, workflow secrets, screenshots, or logs.

## Running

Trigger **Authenticated Windows E2E** manually from GitHub Actions. The workflow first executes the existing TUI, desktop, provider, and workspace tests, then invokes `windows-chatgpt-oauth.ps1`.

The live harness:

1. checks or starts the loopback OAuth gateway;
2. confirms the gateway is reachable without reading its credential files;
3. builds the production CLI, TUI, and Tauri desktop application;
4. runs a real model turn in a disposable Git repository;
5. launches the prompt-driven TUI path and verifies that durable session evidence is written;
6. launches the Windows desktop package as a smoke test;
7. writes only sanitized logs and a JSON result summary.

No access token, refresh token, cookie, authorization header, gateway credential file, or `%APPDATA%\medusa` provider file is uploaded as an artifact.

## Local invocation

```powershell
pwsh -File scripts/e2e/windows-chatgpt-oauth.ps1 `
  -RepositoryRoot . `
  -Prompt "Reply with the exact text MEDUSA_OAUTH_E2E_OK and do not modify files."
```

Use `-SkipDesktopLaunch` on non-interactive Windows sessions. That mode still builds and tests the desktop application but does not claim that the native window was exercised.
