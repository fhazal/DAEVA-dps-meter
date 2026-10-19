# Code signing policy

> **Status:** not yet signed. We plan to apply to [SignPath Foundation](https://signpath.org) for free code signing; until then the installer is unsigned and Windows SmartScreen may warn you (choose **More info → Run anyway**). This policy describes how signing will work once approved.

Daeva Meter's Windows installer (`Daeva.Meter_<version>_x64-setup.exe`) is digitally signed so you can check that it really comes from this project and hasn't been changed since it was built.

## What is signed

- Only the official Windows installer attached to each [GitHub release](https://github.com/fhazal/DAEVA-dps-meter/releases).
- Every signed file is built automatically by [GitHub Actions](.github/workflows/build.yml) from the source code in this repository ([fhazal/DAEVA-dps-meter](https://github.com/fhazal/DAEVA-dps-meter)). SignPath verifies that the file came from that build before signing it.
- Nothing is built or signed on anyone's personal computer.
- Each release is signed only after a team member approves the signing request by hand.

## Team roles

| Role | Member | Responsibility |
|---|---|---|
| Author | [Fhazal](https://github.com/fhazal) | Writes and maintains the source code |
| Reviewer | [Fhazal](https://github.com/fhazal) | Reviews every outside contribution (pull requests) before it is merged |
| Approver | [Fhazal](https://github.com/fhazal) | Approves each release for signing |

All team members use multi-factor authentication for GitHub and SignPath.

## Check a download

In Windows Explorer: right-click the installer → **Properties** → **Digital Signatures**. The signer should be **SignPath Foundation**.

Or in PowerShell:

```powershell
Get-AuthenticodeSignature ".\Daeva.Meter_*_x64-setup.exe" | Format-List Status, SignerCertificate
```

Each release also lists the installer's SHA-256 checksum.

## Privacy

Daeva Meter only **reads** your network traffic to show your damage; it never injects into, modifies or talks to the game client. It sends nothing about you anywhere unless listed below, and each connection can be turned off in Settings:

| Connection | What is sent | Setting |
|---|---|---|
| `api.github.com` | A request for the latest release number | Settings → Data → Updates: *Check GitHub for new versions* |
| `aion2.bajubesi.org` | Requests for the monster/skill list and the event schedule | Settings → Data → Names, and Settings → Alerts |
| `aion2.gaming.tools` | The id of a monster or skill the list doesn't cover, to look up its name | Settings → Data → Names: *Get monster and skill names* |
| The Daeva bot (`aion2.bajubesi.org/api/meter`) | Your finished boss/elite fights (and optionally rare loot), only if you turn sharing on | Settings → Share with Daeva bot (**off by default**) |

*Report a problem* saves a file on your Desktop; it is never uploaded. The installer includes an uninstaller (Windows Settings → Apps).

## Reporting a problem with a signed file

If a file signed for Daeva Meter looks suspicious, don't run it. Open an [issue](https://github.com/fhazal/DAEVA-dps-meter/issues) or tell us in our [Discord](https://discord.gg/TCmm8cH3GE) with the file's SHA-256 and where you downloaded it.
