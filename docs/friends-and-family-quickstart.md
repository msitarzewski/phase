# LUCID Friends & Family Quickstart

Start with LUCID, join a trusted friend's circle, and send your first chat in
about a minute. You do not need an Open WebUI installation.

> **Only accept installers and invitations from people you trust.** A circle
> invitation is a secret key to a private group. Never post one publicly.

## 1. Install LUCID (about 30 seconds)

Download the file for your computer from the
[LUCID Releases page](https://github.com/msitarzewski/phase/releases).

### macOS with Apple Silicon

1. Open the `.dmg` file and drag **LUCID** into **Applications**.
2. Open **LUCID** from Applications. This beta is not Apple-notarized yet, so
   macOS may stop it once with a warning.
3. If macOS asks, choose **Done** in the warning. Then open **System Settings
   > Privacy & Security**, find the LUCID message, and choose **Open Anyway**.
   Alternatively, Control-click **LUCID** in Applications and choose **Open**.
4. Confirm that you want to open it. If you did not download LUCID from the
   Releases page above, stop instead and delete the file.

This warning is expected during the unsigned-beta period. The release file is
still code-signed ad hoc; it is not a substitute for future Apple notarization.

### Ubuntu

1. Download the `.deb` file.
2. Double-click it to open the Software installer, or run:

   ```bash
   sudo apt install ./LUCID_*.deb
   ```

3. Open **LUCID** from your applications list.

### Other Linux

Use the verified command-line installer. It chooses the right Linux release,
downloads `SHA256SUMS.txt`, checks the archive, and only then installs it:

```bash
curl -fsSLo install-lucid.sh \
  https://raw.githubusercontent.com/msitarzewski/phase/main/scripts/install-lucid.sh
bash install-lucid.sh
```

To install a particular release, add `--tag <release-tag>`.

### Windows 11 x86_64

Run this in PowerShell:

```powershell
Invoke-WebRequest `
  https://raw.githubusercontent.com/msitarzewski/phase/main/scripts/install-lucid.ps1 `
  -OutFile "$env:TEMP\install-lucid.ps1"

powershell -ExecutionPolicy Bypass -File "$env:TEMP\install-lucid.ps1"
```

The script verifies the download against the release's `SHA256SUMS.txt` before
it installs `lucidd.exe` under your local app-data folder. Open a new terminal
after installation. To install a particular release, add
`-Tag <release-tag>` to the second command.

## 2. Join your friend's circle (about 5 seconds)

1. Ask your friend to send you a private `lucid://join/...` invitation.
2. Click or tap the invitation on the same device where LUCID is installed.
3. When LUCID opens, review the circle request and choose **Join**.
4. Wait for the connected message. A normal join should take about 3-6 seconds.

If your operating system does not recognize the link, open LUCID and paste it
into **Join Invite**.

## 3. Chat inside LUCID (about 10 seconds)

1. Select an available model in LUCID.
2. Open **Chat**. The Chat window is built into the app; Open WebUI is not
   required.
3. Type a message and press Enter or choose **Send**.
4. Leave **Local Only** on if the request must stay on your own computer. Turn
   it off only when you intentionally want the approved circle to serve it.

## Optional: safely share a model

Sharing is **off by default**. LUCID asks before it makes your model or hardware
available.

To share:

1. Choose a model stored on your computer.
2. Select **Provide** and read the permission prompt.
3. Turn sharing on only if you want invited circle members to use it.

LUCID can auto-pause sharing on battery power or high device temperature. You
can pause or stop providing at any time. Pausing returns your hardware to
private use; it does not revoke an already completed response.

## Trust & privacy: in plain English

- **Your friend can see the prompt on a serving GPU.** If you send a request to
  a friend's device, that GPU's operator and software receive the prompt and
  response. Transport encryption and receipt verification do not hide plaintext
  from the machine doing the work.
- **Local Only is a routing boundary.** It keeps the chat request from being
  sent to a remote peer. It is the right choice for a message that must not
  leave your computer.
- **`trusted-circle-sharded` is not “private cloud.”** It means the approved
  circle split the work across trusted devices. Trust each approved operator
  and device as you would a single serving GPU.
- **Receipt verification is attribution, not correctness proof.** It checks
  that a serving identity signed the delivered output and transcript. It does
  not prove the peer ran the exact model correctly.
- **Invitations are private.** Do not paste a `lucid://join/...` link into a
  public chat, ticket, screenshot, or social post. Ask your friend for a new
  invitation if one was exposed.

## If it does not work

- Confirm you opened the installer from the official Releases page.
- Confirm the invitation was copied completely and is still private.
- Keep the LUCID window open until the Join confirmation appears.
- Ask the circle owner whether the invitation expired or has already been used.
- For sensitive prompts, stop and enable **Local Only** before retrying.
