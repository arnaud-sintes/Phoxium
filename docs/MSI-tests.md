# MSI laptop test session

Step by step, for the owner's second PC, an MSI GS66 Stealth 10UE-209FR (Intel Core i7-10870H, RTX 3060 Laptop 6 GB, Intel UHD Graphics 630, a 15.6 inch 1080p panel, 300 Hz according to the French retailers). About 60 to 90 minutes. The reasons for each test are the items of `docs/TODO.md` ("Can be tested on the MSI laptop"); the shorter reference version is `docs/ENV.md` section 9. If a step fails, skip it and carry on: which step failed is useful. Send back the text of the files named below (they are small); screenshots are optional.

## 0. Setup (5 min)

Open PowerShell on the MSI:

```powershell
mkdir C:\phoxium-test; cd C:\phoxium-test
$u = 'https://github.com/arnaud-sintes/Phoxium/releases/download/v0.9.4'
Invoke-WebRequest "$u/phoxium-v0.9.4-windows-x64.zip" -OutFile p.zip
Invoke-WebRequest "$u/phoxium-v0.9.4-windows-x64.zip.sha256" -OutFile p.sha
(Get-FileHash p.zip -Algorithm SHA256).Hash; Get-Content p.sha     # the two must match
Expand-Archive p.zip .
Invoke-WebRequest https://raw.githubusercontent.com/arnaud-sintes/Phoxium/main/scripts/collect-hardware-info.ps1 -OutFile collect-hardware-info.ps1
```

- **Test source:** `pixel-demo.exe` is not in the zip. Copy it from the development PC (`C:\projects\personal\Phoxium\target\release\pixel-demo.exe`) into `C:\phoxium-test`. If you cannot, use a video playing in a browser and say so.
- **Running Phoxium:** it is a window-subsystem program, so for the steps that save its output use `Start-Process ... -Wait`, as below.
- **Windows warning:** SmartScreen may warn on the first start (the build is unsigned). Click "More info", then "Run anyway", only after the hash matched. Write down whether the warning came up.

## 1. Inventory (5 min)

```powershell
.\collect-hardware-info.ps1 -PhoxiumExe .\phoxium-v0.9.4-windows-x64\phoxium.exe
```

Send `hardware-info.txt`. It holds no name and no serial number. It settles the facts the web pages disagree on (the RAM, the Windows edition, the refresh rates the panel offers, the Vulkan devices Phoxium sees, whether the UHD 630 offers Vulkan 1.3).

## 2. Graphics mode and the stats run (15 min)

In Dragon Center (or MSI Center) note whether GPU Switch is on **MSHybrid** or **Discrete**. Start in MSHybrid; if the laptop is in Discrete, do step 6 first.

```powershell
$x = '.\phoxium-v0.9.4-windows-x64\phoxium.exe'
Start-Process $x -ArgumentList '--list-adapters' -RedirectStandardOutput adapters-hybrid.txt -Wait
Start-Process .\pixel-demo.exe -ArgumentList '--native','1280x720','--present','1to1','--content','motion','--fps','60','--no-toast','--exit-after','100'
Start-Sleep 3
Start-Process $x -ArgumentList '--window','pixel-demo','--stats','--preset','slot-tv-26in','--exit-after','60' -RedirectStandardOutput stats-hybrid-slot.txt -RedirectStandardError err-hybrid-slot.txt -Wait
```

Send `adapters-hybrid.txt` and `stats-hybrid-slot.txt`. Looked at: the GPU chosen, which GPU owns the screen, the `gpu ... ms` and `render ... fps` lines. This is the first measure of the cross-adapter cost (`QUESTIONS.md` HW-3).

## 3. High refresh (15 min)

In Windows Settings, System, Display, Advanced display, set the refresh rate to the highest available (300 Hz?). Run the stats command of step 2 twice with different output files: once with `--preset slot-tv-26in` (`stats-300-slot.txt`) and once with `--preset pvm-20m4u` (`stats-300-pvm.txt`). Then set the panel to 60 Hz and repeat both (`stats-60-slot.txt`, `stats-60-pvm.txt`). Send the four files.

During one run press `Ctrl+Alt+F9` (the overlay) and take a screenshot (Win+Shift+S): it shows the refresh rate and the quality tier in force. Looked at: the `notice: quality lowered...` lines, whether the drop stops at `low`, whether it flaps (a `quality back to...` line within a minute), the render rate against the refresh rate, stutter.

## 4. File dialogs (5 min)

Double-click `phoxium.exe`, pick a window in the list, press F10. In the menu: the presets section, Manage files, **Export** (a Save dialog), Esc, then **Import** (an Open dialog), Esc. Then Source, **Open image**, Esc. Write down any delay over 3 seconds, any dialog that never opens or never closes, any freeze, and the shell extensions installed (a cloud-drive client, an archive manager).

## 5. Cameras (10 min)

In the source picker, under **Capture card**, write down every device name (there should be an RGB camera and an IR camera). Open each one:

- Is the picture the right way up and not skewed?
- What does the window or the console say about the format?

Then switch the webcam off, with the Fn webcam key (check the key icons) or in Settings, Privacy & security, Camera. With a camera open, and again trying to open one while it is off:

- Does the window keep responding?
- Does the "opening" message end by itself within about 8 seconds?
- Do Esc and Change source work?
- Does `Ctrl+Alt+End` close the program within 2 seconds?

Write down what happened.

## 6. Discrete mode (15 min)

In Dragon Center set GPU Switch to **Discrete** and restart. Repeat the commands of step 2 with the file names `adapters-discrete.txt` and `stats-discrete-slot.txt`, and one run at the highest refresh rate (`stats-discrete-300-slot.txt`). Send them.

## 7. Notes to send with the files

- The Windows edition and build, and whether it is Windows 10 or 11. If it is Windows 10: did everything start and capture normally, or what failed? (The README requires Windows 11; this decides whether that is a real limit or only the tested one.)
- Whether SmartScreen warned on the first start, and whether the first-run hint appeared.
- The size and position of the window that opens by default; whether F11 and back worked.
- Anything odd: stutter, tearing, wrong colours, a crash.

## Files to send back

| Step | Files |
|---|---|
| 1 | `hardware-info.txt` |
| 2 | `adapters-hybrid.txt`, `stats-hybrid-slot.txt` |
| 3 | `stats-300-slot.txt`, `stats-300-pvm.txt`, `stats-60-slot.txt`, `stats-60-pvm.txt`, an overlay screenshot |
| 4 and 5 | your notes (no file) |
| 6 | `adapters-discrete.txt`, `stats-discrete-slot.txt`, `stats-discrete-300-slot.txt` |
| 7 | your notes |

The results go into `docs/QUESTIONS.md` and the matching items of `docs/TODO.md` are ticked or reopened.
