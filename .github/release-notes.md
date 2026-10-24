**Daeva Meter** — a passive damage meter for AION 2, by BajuBesi Labs. See the [changelog](https://github.com/fhazal/DAEVA-dps-meter/blob/main/CHANGELOG.md) for everything in this version · [Code signing policy](https://github.com/fhazal/DAEVA-dps-meter/blob/main/CODE_SIGNING.md)

### Install
1. Install **[Npcap](https://npcap.com/#download)** with the default options, and leave *"Restrict Npcap driver's access to Administrators only"* **unticked**.
2. Download **`Daeva.Meter_…_x64-setup.exe`** below and run it. It isn't code-signed yet, so Windows SmartScreen may say *"Windows protected your PC"*: choose **More info → Run anyway**.
3. Start AION 2 in borderless or windowed mode, then Daeva Meter. Updating: run the new installer over the old one.

Daeva Meter only **reads** your network traffic. It never injects into, modifies or talks to the game client; its only contact with the game server is a ping check once a minute (it opens a connection and closes it, sending no data). Unofficial fan project, not affiliated with NCSOFT. Includes monster names from [A2Tools](https://github.com/taengu/A2Tools-DPS-Meter) by taengu, under the GPL-3.0 (installed in `definitions\a2tools` with its licence).

### Verify your download
Only download from this page. To check the file is genuine, run this in PowerShell in your Downloads folder and compare the result with the SHA-256 below:
`Get-FileHash ".\Daeva*Meter_*_x64-setup.exe" -Algorithm SHA256`
