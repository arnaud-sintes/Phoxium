# CRT.md: real-life CRT knowledge base for Phoxium

Scope: direct-view colour CRTs (PC monitors, consumer TVs, broadcast PVM/BVM, arcade), seen as the inputs a physically based GPU model needs.
Goal: turn source RGB pixels into the light a real tube would emit.
This is not a software design document.

## Provenance and confidence tags

This file reconciles three independent research passes (Sonnet, Opus, Fable agents, high effort, web access, 2026-11-18).
Where all three agree, the statement is stated plainly.
Where they disagree, the disagreement is listed in "Resolved conflicts" or "Unresolved uncertainties and data gaps".

| Tag | Meaning |
|---|---|
| [S] | Read in a fetched source (datasheet, patent, paper, service manual, forum measurement, repo) by at least one agent |
| [D] | Derived by calculation from tagged inputs, or from geometry/physics |
| [K] | Standard or textbook knowledge (ITU, SMPTE, EBU, basic physics), not re-verified this pass |
| [E] | Engineering estimate, community lore or memory. Trust the order of magnitude, not the last digit. Expose as a tunable |

Many web fetches failed (403, binary PDFs).
Vendor pitch tables, decay constants, spot sizes and glass transmissions are the weakest data.
Treat every [E] number as a default for a slider, never as a specification.

## Ten things the shader must get right

1. **Order of operations.** The video amplifier acts horizontally on the gamma-encoded voltage before the gun power law. Blurring in linear light is wrong. The measurable consequence is the adjacent-pixel nonlinearity: isolated 1-pixel vertical lines come out 5 % to over 50 % dimmer than horizontal ones.
2. **The beam spot grows with brightness.** Scanlines vanish when beam FWHM reaches about one line pitch. Dark lines stay thin and bright lines fatten, so a constant beam width is wrong.
3. **The mask samples the beam, it does not multiply an image.** Each phosphor element of colour c lights in proportion to beam c at its location. Moiré and chromatic fringing then come for free, but only if the host display can resolve the triads.
4. **The mask eats most of the light.** It stops about 75 to 85 % of the beam. A pixel-level mask on a flat panel therefore needs HDR headroom (about 3 to 7 times the CRT white, estimate).
5. **Host pixels are the bottleneck.** A PVM-class mask (0.25 to 0.31 mm pitch) gives only about 2.3 to 2.7 host pixels per triad on a 4K panel for a 4:3 image. At least 3 are needed. Below that the mask must be area-filtered or enlarged, otherwise host-grid moiré appears that no real tube ever showed.
6. **Light is emitted in short pulses.** At 60 Hz a 100 cd/m² average implies about 10³ to 10⁴ cd/m² instantaneous at the beam. No panel reproduces that, so rolling-scan simulation trades peak brightness against motion clarity.
7. **Halation is a ring, not a blur.** Light trapped in the faceplate returns at a radius of about 1.75 times the glass thickness (about 17 to 25 mm on a typical tube).
8. **Phosphor decay is colour dependent and non-exponential.** Red is near-exponential (about 1 to 2 ms). Green and blue drop within microseconds, then keep a heavy power-law tail (exponent about 1.1) that puts about 5 % of a frame's light into the next frame at 60 Hz (one measured monitor, see "Decay and persistence").
9. **A real CRT does not colour-convert.** Source RGB values drive the cathodes directly and the tube's own primaries and white point define the colour. A faithful model reinterprets RGB, with a configurable source EOTF.
10. **Almost everything about an individual consumer set is unmeasured.** Pitch, spot size, decay, curvature and glass are weakly documented. Design every one of them as a parameter with a preset, not a constant.

## Physical chain, in the order physics applies it

1. Video voltage per channel (gamma-encoded, 0 to 0.7 V) is low-pass filtered and slew-limited by the video amplifier, horizontally only, in the voltage domain. [S]
2. Brightness (offset) and contrast (gain) act on that voltage. Per-gun bias and drive trim cutoff and gain. [S]
3. Each gun turns drive voltage into beam current by a power law, exponent about 2.3 to 2.6. [S]
4. The beam is deflected along a raster. Geometry errors distort its landing position, and R, G, B land slightly apart (misconvergence). [S]
5. The beam current density is roughly Gaussian. Its width grows with beam current and with deflection angle. [S]
6. A mask or grille 10 to 15 mm behind the screen passes about 15 to 27 % of the electrons and assigns each beam to its own colour of phosphor. [S][D]
7. Phosphor elements (dots, slots or stripes inside a black matrix) emit light with fixed primaries, a small saturation at high current density and a colour-dependent decay. [S]
8. Light crosses 10 to 20 mm of usually tinted glass. Part of it is trapped (halation). Electrons scatter inside the tube (veiling glare). [S]
9. The outer surface reflects ambient light specularly and the phosphor screen reflects it diffusely. [S]
10. All of this happens one line at a time. At any instant only a band of a few to a few tens of lines glows. [S][D]

Steps 1 to 3 live in voltage space.
Steps 5 to 9 live in linear light.

## Tube anatomy and operating point

### Electrical operating point

| Quantity | Value | Tag |
|---|---|---|
| Anode (EHT) voltage, colour tubes | 24 to 32 kV; a 21 in monitor tube datasheet uses 27.5 kV nominal, 29.9 kV max; small 14 in tubes about 22 to 25 kV | [S][E] |
| Electron speed at 25 kV | about 0.3 c (non-relativistic formula within a few percent) | [K] |
| Focus electrode | about 25 % of anode voltage (6.8 to 7.5 kV at 27.5 kV) | [S] |
| G2 (screen grid) | several hundred volts; sets gun cutoff | [S] |
| Video drive at cathodes | 40 to 170 V peak-to-peak per channel | [S] |
| Beam current | about 0.3 mA normal cathode loading; datasheet tests at 100 to 400 µA per gun; consumer TVs average about 1 to 1.5 mA total with peaks of several mA | [S][E] |
| Deflection angle | 90° monitors and small tubes, 110° large TVs, 120 to 125° late slim tubes (quoted on the diagonal; a 90° tube is 78° horizontal, 60° vertical) | [S] |
| Aluminium backing | costs the beam about 1 kV, reflects phosphor light forward, forms the anode contact | [S] |
| Beam speed across screen | about 7.6 mm/µs (20 in TV at 15.7 kHz), 11 mm/µs (15 in VGA at 31.5 kHz), about 58 mm/µs (21 in at 106 kHz) | [D] |
| Dwell time on a phosphor point | about 30 to 130 ns on a TV, under 10 ns on a high-end monitor | [D] |
| Deflection sensitivity | scales as 1/sqrt(V_anode): lower EHT enlarges the raster | [K] |
| Line rates | 15.6 kHz (TV) up to above 100 kHz (high-end monitors) | [S] |
| Power on the mask | over two thirds of the beam energy; 25 W beam, about 17 to 20 W lands on the mask | [S][E] |

### Electron guns

| Gun | Used in | Simulation notes |
|---|---|---|
| Delta (three guns in a triangle) | Early colour TVs, many 1980s/90s PC monitors with a "dot pitch" | Pairs with round-hole mask and hexagonal triads. Needs dynamic convergence. Isotropic resolution. |
| In-line, three lens systems | Nearly all consumer TVs from the mid-1970s, many monitors | Self-converging yoke. Corner spot is horizontally elongated with a vertical haze unless a dynamic astigmatism gun corrects it. |
| Trinitron single gun, three in-line cathodes, one large lens | Sony Trinitron TVs, PVM/BVM, GDM/CPD monitors | Large common lens gives lower aberration and a smaller spot. Pairs with an aperture grille. |
| Diamondtron (Mitsubishi) | NEC/Mitsubishi monitors | Aperture grille with in-line guns of its own design. |

Gun spacing is typically 5 to 7 mm.
A worked example uses 7 mm gun spacing, 350 mm deflection-centre-to-mask and 15 mm mask-to-screen distance. [S]

## Colour selection structures

### Types

| Type | Openings | Phosphor layout | Vertical structure | Typical users |
|---|---|---|---|---|
| Delta shadow mask (dot) | Round holes on a hexagonal lattice | Round dots in R/G/B triads | Yes | Many PC monitors, JVC and Ikegami broadcast monitors, early TVs |
| Slot mask (in-line) | Vertical slots with bridges, columns staggered by half a slot | Vertical stripes or slot-shaped islands | Yes (bridges) | Most consumer TVs outside Sony, NEC CromaClear (elliptical slots), LG Flatron |
| Aperture grille | Continuous vertical slits between tensioned wires | Continuous vertical stripes plus black matrix | None, except damper wires | Sony Trinitron, Mitsubishi Diamondtron, PVM/BVM |

- NEC CromaClear is a slot mask with elongated elliptical slots and a 0.25 mm mask pitch. [S]
- LG Flatron monitors 915FT Plus and F900P are listed as slot mask, 0.24 mm. [S]
- Mitsubishi Diamondtron and Sony Trinitron are both aperture grilles. Treat Diamondtron as the same cell geometry with different pitch and transmission. [S][E]
- Zenith Flat Tension Mask (1986) is a tensioned foil shadow mask behind a flat faceplate. [K]

### Pitch definitions (not comparable across types)

- **Dot (delta) mask:** "dot pitch" P is the distance between nearest same-colour dots, measured along a diagonal at 30° above horizontal. [S]
- **Vendor "horizontal dot pitch"** is P × 0.866. [S]
- **Aperture grille:** "stripe pitch" is the horizontal distance between same-colour stripes (one triad). [S]
- **Slot mask:** "slot pitch" is horizontal, sometimes quoted at the mask rather than at the phosphor. [S]
- Marketing pitch is measured at screen centre, and edge pitch is usually larger.

### Delta (dot) triad geometry

Using the sourced geometry (Goldwasser), for diagonal dot pitch P:

| Quantity | Formula | Value for P = 0.28 mm |
|---|---|---|
| Same-colour repeat along one row | P·√3 | 0.485 mm |
| Same-colour horizontal spacing between adjacent rows ("horizontal dot pitch") | P·√3/2 | 0.242 mm |
| Vertical spacing of dot rows | P/2 | 0.140 mm |
| Spacing of adjacent dots of any colour in a row | P/√3 | 0.162 mm |

- Along a row the colours run R G B R G B with spacing a = P/√3.
- Each row is shifted by 1.5 a relative to the previous one.
- Dot diameter is somewhat below a because of the black matrix (about 0.4 to 0.5 P as an estimate). [E]
- A round hole of diameter h on a triad cell of area A_t transmits about π h² / (4 A_t). [D]

### Slot mask geometry

- Triad horizontal period p, with each colour stripe p/3 wide. [D]
- Vertical slot period is typically 2 to 3 times the stripe spacing p/3 on TVs, with bridges about 10 to 20 % of that period. [E]
- Adjacent columns are staggered by half the slot period (brick pattern). [S]
- On most TVs the phosphor is in continuous vertical stripes and the brick pattern on screen is the shadow of the bridges. [E]
- Bridge shadows modulate brightness with a vertical period of the slot row.

### Aperture grille geometry

- Continuous vertical R, G, B stripes separated by black guard stripes, no vertical structure. [S]
- About a thousand tensioned vertical wires on a monitor tube. [S]
- **Damper wires:** tungsten, 10 to 30 µm diameter (15 to 30 µm in another Sony patent), stretched horizontally. [S]
- Count: one wire about 2/3 of the way down on tubes of 15 in and below, two wires at 1/3 and 2/3 on larger monitors. [S] Goldwasser reports three wires above 21 in. [S]
- Their shadows are faint dark horizontal lines, most visible on bright uniform fields and less visible on SD sets where scanlines are thick. [S]
- Trinitron tubes usually have variable pitch (finest at centre, coarser at edges). Diamondtron tubes have uniform pitch. [S]
- Without horizontal bridges, vertical profile is set only by scanline spacing and spot, so there is no scanline/mask vertical moiré.
- Trinitrons are cylindrical (curved horizontally, flat vertically) because the strips are straight. [S]

### Mask-to-screen spacing and pitch magnification

- The three beams cross the mask at different angles, so each lands on its own colour. [S]
- With deflection-centre-to-mask distance L, gun spacing s and screen pitch p: q ≈ L·(p/3)/s. The exact form is q = L·p/(3s + p). [D]
- Example: L = 350, p = 0.9, s = 7 gives q = 15 mm.
- Screen pitch is magnified relative to mask pitch by about L/(L − q), roughly 1.01 to 1.05 times at centre, more toward the edges. [D]
- This is why spec sheets differ on whether "pitch" is the mask or the screen value.

### Beam transmission and brightness budget

| Electrode | Fraction of beam reaching phosphor | Tag |
|---|---|---|
| Round-hole shadow mask (centre) | about 15 to 20 % (a patent states the mask intercepts all but about 18 %) | [S] |
| Slot mask | about 20 % (up to about 25 to 30 %) | [S][E] |
| Aperture grille | about 20 to 27 %, geometric ceiling 1/3 | [D] |
| Mask edges and corners | lower than centre | [E] |

- A shadow-mask set needs guns about five times more powerful than a monochrome set of the same brightness. [S]
- Consequence for an emissive model: each primary emits from only 15 to 27 % of the screen area. Total phosphor coverage (all three colours) is about 35 to 60 %, so a lit element is 1.7 to 3.3 times (area-averaged over all colours) or 4 to 6 times (one primary only) brighter than the area-average. [D][E]
- To reproduce 100 cd/m² CRT white with a pixel-level mask and visible scanlines, a flat panel needs roughly 400 to 700 cd/m² peak white (estimate). That is why HDR-based shaders such as Sony Megatron exist. [D][E]

### Black matrix and phosphor screen

- Black matrix (Zenith 1969, Panasonic 1970) fills the gaps between phosphors. Phosphor elements are smaller, but the matrix absorbs ambient light, so a clearer faceplate can be used and net brightness and contrast go up. [S]
- In matrix tubes the beamlet through each mask opening is larger than the matrix opening ("negative guard band"). The luminous shape of an element is therefore the matrix opening with nearly uniform fill, not the beamlet shape. [S][E]
- Phosphor coverage of screen area is roughly 35 to 60 %. [E]
- Stripe fill for a grille is about 0.2 to 0.27 of the triad pitch per colour, with black guard about 0.06 to 0.13 p. [E]
- Phosphor dot edges are soft (about 5 to 10 % of pitch). [E]
- The mask is also the exposure stencil during phosphor deposition, which is why mask and screen are a paired unit. [K]

### Doming, purity and thermal behaviour

- A shadow mask runs at 80 to 90 °C and can move up to about 100 µm through expansion (doming). [S] Other estimates give 10 to 30 µm landing shift. [E]
- Invar expands about 7 times less than iron and tolerates 2 to 3 times more beam current for the same purity error. [S]
- An aperture grille expands only vertically, which does not shift colours. [S]
- The guard band in black-matrix tubes is about 15 to 30 µm. [E]

## Pitch values by family

### Sony professional (verified from third-party databases and manuals)

| Model | Size | Structure | Pitch (mm) | TVL rating | Tag |
|---|---|---|---|---|---|
| BVM-14E1/E5 | 14 in | AG | 0.22 | n/a | [S] |
| BVM-14F1/F5 | 14 in | AG | 0.25 | 800 | [S] |
| BVM-20E1U | 20 in | AG (HR Trinitron) | 0.25 | 1000 | [S] |
| BVM-20F1U / A20F1U / D20F1U | 20 in | AG | 0.30 | 900 | [S] |
| BVM-D24E1WU | 24 in 16:9 | AG | 0.25 to 0.28 variable | n/a | [S] |
| PVM-14L1 / 14L2 | 14 in | AG | 0.25 | 600 | [S] |
| PVM-14M4U | 14 in | AG | 0.25 | 800 | [S] |
| PVM-20L1 / 20L2 | 20 in | AG | 0.40 | 600 | [S] |
| PVM-20M4U | 20 in | AG | 0.31 | 800 | [S] |
| PVM-20S1WU | 20 in | AG | 0.65 | n/a | [S] hobbyist database, looks coarse |
| PVM-2530 | 25 in | AG | 0.73 | n/a | [S] hobbyist database |

Cross-check: Sony TVL ratings match picture height / pitch to within about 15 % (290 / 0.31 = 935 against 800; 290 / 0.25 = 1160 against 1000). [D]

### PC monitors

| Model / family | Structure | Pitch (mm) | Tag |
|---|---|---|---|
| Sony GDM-F500 / F520 (21 in FD Trinitron) | AG | 0.22, uniform | [S] |
| Sony GDM-FW900 (24 in 16:10) | AG | 0.23 to 0.27 variable | [S] |
| Sony GDM-W900 | AG | 0.25 to 0.28 variable | [S] |
| Sony CPD-G400 / E210 / G220S / G520P | AG | 0.24 to 0.25 | [S] |
| Mitsubishi Diamond Pro 2060u / 2070SB, NEC FP2141SB (22 in) | AG (Diamondtron NF) | 0.24, uniform | [S] |
| Mitsubishi Diamond Pro 91TXM | AG | 0.28 | [S] |
| LaCie electron19blue III (Diamondtron) | AG | 0.24 (42 triads in 10 mm, measured) | [S] |
| Iiyama Vision Master Pro 17 | AG | 0.26 | [S] |
| NEC CromaClear 15 to 17 in | slot (elliptical) | 0.25 (mask pitch) | [S] |
| LG Flatron 915FT Plus / F900P (19 in) | slot | 0.24 | [S] |
| Entry-level 14 to 15 in | dot | 0.28 diagonal, 0.24 to 0.25 horizontal | [S] |
| Good 17 to 19 in | dot | 0.26 diagonal, 0.22 horizontal | [S] |
| Low-end 14 in | dot | 0.39 | [S] |
| Early CGA/EGA/VGA 14 in | dot | 0.31 to 0.43 | [E] |
| Hitachi "EDP" 19 to 21 in | dot, compressed triads | about 0.21 to 0.22 horizontal | [E] |
| Samsung DynaFlat, Panasonic, Toshiba | dot or slot | about 0.20 to 0.28 | [E] |

### Consumer TVs and arcade

| Set | Structure | Pitch (mm) | Tag |
|---|---|---|---|
| 13 in GE TV | slot | 0.60 (measured by Goldwasser; about 440 triads across per crt-royale README) | [S] |
| 19 in Samsung TV | slot | 0.75 (about 515 triads) | [S] |
| 25 in RCA TV | slot | 0.90 (stripe spacing 0.30; about 564 triads) | [S] |
| 21 in CCTV (Samsung) | stripe | 0.70 (about 610 triads) | [S] |
| Panasonic 9 in tube A22JWG098X | stripe | 0.50 | [S] |
| Unidentified 19 in arcade monitor | dot or slot | 0.81 | [S] forum |
| RCA A36ACG10X, 36 cm (14V), 90° | slot, line screen | 0.66 | [S] RCA 1983 datasheet |
| RCA A51ACG20X, A51ABU10X, A51ACC10X, 51 cm (20V), 90° and 110° | slot, line screen | 0.84 | [S] RCA 1983 datasheet |
| RCA A66ABU10X, 66 cm (26V), 110° | slot, line screen | 0.82 | [S] RCA 1983 datasheet |
| RCA A68ACC10X, 68 cm (27V), 110° | slot, line screen | 0.84 | [S] RCA 1983 datasheet |
| 13 to 14 in TV | slot or AG | 0.45 to 0.66 | [E] (upper end [S] RCA) |
| 19 to 21 in TV, 19 in arcade | slot | 0.6 to 0.85 | [E] (upper end [S] RCA) |
| 25 to 29 in TV | slot | 0.75 to 0.9 | [E] (RCA 0.82 to 0.84 [S]) |
| 32 to 36 in TV | slot or AG | about 0.8 to 1.0 | [E] |

General ranges: PC monitors 0.22 to 0.28 (0.39 at the low end), all colour CRTs 0.22 to 0.40, up to 0.80 or more on the largest. [S]
Phosphor grain becomes noticeable at 50 to 65 cm viewing when pitch exceeds about 0.35 mm. [S]

### Visible size and triad counts

- Viewable diagonal is about 1 inch less than nominal. 4:3 width = 0.8 × diagonal, height = 0.6 × diagonal. [S]
- Triads across N = visible width / horizontal pitch. [S]
- Useful horizontal resolution in pixels is about N. A spot-to-pixel ratio of 1.5 to 2:1 gives the best perceived sharpness. [S]
- For equal resolution, stripe pitch must be about 90 % of dot pitch. [S]

Reference scales for 4:3 sources, viewable diagonal = nominal − 1 in:

| Tube | Visible w × h (mm) | Source format | Line pitch (mm) | Triad pitch (mm) | Triads across | Triads per source pixel |
|---|---|---|---|---|---|---|
| 14 in BVM | 264 × 198 | 320 × 240 | 0.83 | 0.25 | 1056 | 3.3 |
| 20 in BVM-20F1U | 386 × 290 | 320 × 240 | 1.21 | 0.30 | 1287 | 4.0 |
| 20 in PVM-20M4U | 386 × 290 | 320 × 240 | 1.21 | 0.31 | 1245 | 3.9 |
| 20 in consumer TV | 386 × 290 | 320 × 240 | 1.21 | 0.65 | 594 | 1.9 |
| 26 in viewable consumer TV | 528 × 396 | 320 × 240 | 1.65 | 0.80 | 660 | 2.1 [D] |
| 15 in VGA monitor | 284 × 213 [D] | 640 × 480 | 0.44 | 0.242 horizontal | 1176 [D] | 1.8 |
| 15 in VGA, 320 × 200 doubled | 284 × 213 [D] | 320 × 200 | 0.53 per scanline | 0.242 horizontal | 1176 [D] | 3.7 [D] |
| 17 in monitor | 325 × 244 | 1024 × 768 | 0.32 | 0.25 | 1300 | 1.27 |
| 21 in monitor | 406 × 305 [D] | 1600 × 1200 | 0.25 | 0.22 | 1847 [D] | 1.15 [D] |

- [D] T-14 follow-up: the 15 in and 21 in rows now follow the stated rule (14 in: 284.5 × 213.4 mm, 284.5 / 0.242 = 1176; 20 in: 406.4 × 304.8 mm, 406.4 / 0.22 = 1847, 1847 / 1600 = 1.15; 1176 / 320 = 3.7). The 26 in row: 660 / 320 = 2.06, rounded 2.1.

For a 256-wide source, multiply triads per source pixel by 1.25.
Line pitch at 480 lines is half the 240-line value.
Triads per source pixel drives the whole look.

### Host-display resolution budget

- To draw each R/G/B element as at least one host pixel, at least 3 host pixels per triad are needed horizontally. About 6 or more are needed to show stripe and guard widths. Dot and slot masks need about 6 to 8 for recognisable shapes. [D]
- A 4:3 image filling a 2160-line 4K panel is 2880 px wide:
  - a 26 in viewable consumer TV mask (about 660 triads) gets 4.4 px per triad: resolvable;
  - a PVM-20M4U mask (1245 triads) gets 2.3 px per triad: not resolvable;
  - a PVM-14 mask (1056 triads) gets 2.7 px per triad: not resolvable.
- On 1080p (1440 px wide for 4:3), even the 26 in TV mask gets only about 2.2 px per triad.
- crt-royale's README says shadow masks need 4K to 8K output. [S]
- Masks below about 3 px per triad must be area-integrated (prefiltered) or enlarged (a "cell size" parameter), otherwise host-grid moiré appears. That moiré is not a real CRT artefact. [D]
- The limit applies per axis. The vertical mask structure (slot rows of period V / 2 once averaged over x, delta rows of period P / 2) is the same for the three colours, so its limit is the sampling theorem's 2 host px per period, not 3. A delta mask at 3 px per triad has rows every 1.73 px and a slot mask with V = 0.83 p rows every 1.25 px: both alias vertically (27 % and 6 % ripple of the row means) although they resolve horizontally. [D]
- An alternative is to use the host display's own subpixel layout (Sony Megatron approach), which needs knowledge of the panel (RGB stripe, BGR, WOLED, QD-OLED) and is panel-specific.

### Moiré

- Moiré is the beat between two periodic structures. [S]
- **Scan moiré:** scanline pitch against the vertical period of a dot or slot mask. Aperture grilles cannot show it. [E]
- **Video moiré:** pixel pitch along the line against the horizontal triad pitch. Every tube can show it. [E]
- Beat period for two pitches p1 and p2: Λ = p1·p2 / |p1 − p2|. [D]
- Visibility threshold for moiré modulation is about 1.4 % (Barten). In one patent, enlarging the spot about 14 % (0.7 to 0.8 mm) pushed moiré under the threshold. [S]
- A small sharp spot aggravates moiré. A spot of at least 1.5 pitches suppresses it. Contrast scales with the spot MTF at the pixel frequency, exp(−2π²σ²/p²). [D]
- Monitors had a "moiré cancel" control that dithers position by a fraction of a pixel on alternate frames. [E]
- Rule of thumb from tube design: scanline pitch to vertical triad spacing of about 1.5:1 keeps moiré low. [S] weak

## Electron beam and scanlines

### Spot profile

- Measured beam cross-sections are "substantially Gaussian". A 21 in tube datasheet says the spot "approximates the Gaussian distribution". A square of side 4σ contains 90 % of the flux. [S]
- Real spots deviate: aberration, space charge and aperture clipping give heavier-than-Gaussian tails. [S][E]
- A good model is a Gaussian core plus a wider Gaussian or exponential tail at about 2 to 10 % weight, or a generalised Gaussian exp(−|x/w|^p) with p about 2 at low current and 2.3 to 2.8 at high current. [E]
- Manufacturer "spot size" usually means width at half peak (FWHM). [S]

| Width definition (Gaussian) | Value |
|---|---|
| FWHM (50 %) | 2.355 σ |
| Full width at 60 % (shrinking-raster method) | 2.02 σ |
| Full width at 10 % | 4.292 σ |
| Full width at 5 % | 4.895 σ |
| "e⁻²" width | 4 σ |
| Square containing 90 % of flux | 4 σ side |

- MTF of a Gaussian spot: MTF(f) = exp(−2π²σ²f²), f in cycles/mm. [K]

### Measured and estimated spot sizes

Hard data points:

| Tube | Condition | Centre (mm) | Corner (mm) | Tag |
|---|---|---|---|---|
| 21 in high-res tube, 27.5 kV | 100 µA, width at 50 % | 0.4 × 0.4 | 0.6 × 0.4 | [S] |
| same | 200 µA | 0.5 × 0.5 | 0.6 × 0.5 | [S] |
| same | 400 µA | 0.6 × 0.6 | 0.7 × 0.5 | [S] |
| 40 cm colour tube, 0.28 pitch | width at 10 %, current unit ambiguous | 0.65 (FWHM about 0.36, σ about 0.15) | n/a | [S] |
| Unspecified VGA monitor (patent) | n/a | 0.7 to 0.8 | n/a | [S] |

Working defaults at screen centre (FWHM, mid drive; estimates, treat as tunable):

| Display class | FWHM at mid drive | FWHM at peak white | Tag |
|---|---|---|---|
| PC monitor 15 to 21 in | 0.3 to 0.5 mm | 0.4 to 0.6 mm | [S][E] |
| PVM / BVM 14 to 20 in | 0.4 to 0.7 mm | 0.7 to 1.1 mm | [E] |
| Consumer TV 20 to 27 in | 0.7 to 1.5 mm | 1.5 to 2 mm (up to 3 at extreme drive) | [E] |

- Corner multiplier: 1.3 to 2 times horizontally, 1.0 to 1.3 times vertically (with dynamic focus); up to 1.5 to 2.5 times without it. Corner spots are elliptical (axis ratio 1.2 to 3:1 on cheap sets). [S][E]
- A datasheet allows astigmatism of 1:1.1 at the centre and 1:1.4 over the useful screen, and a spot size of up to about twice the dot pitch in the corners. [S]
- See "Resolved conflicts" for why the Opus agent's lower spot sizes were not adopted.

### Spot growth with beam current

- Space-charge repulsion at the crossover enlarges the virtual object, and spherical aberration adds a term. Best-focus voltage also shifts with current. [S]
- Datapoints: 0.4, 0.5, 0.6 mm at 100, 200, 400 µA (exponent about 0.29). [S][D] Projection-gun rule of thumb: diameter ∝ √I, about +40 % from 1 to 2 mA. [S]
- Colour-tube datapoints (T-63, all from patents; the measurement level is not stated unless given):
  - Hitachi 51 cm colour monitor tube, 0.25 mm mask pitch: 0.45, 0.57, 0.70 mm at 100, 300, 500 µA cathode current (US 5,814,930). Exponent about 0.27 overall (0.22 from 100 to 300 µA, 0.40 from 300 to 500 µA). The patent calls 500 µA the "maximum allowable" current of the monitor and says moiré is not seen above 0.45 mm. [S][D]
  - Panasonic 76 cm, 106° colour TV: smallest spot about 0.9 mm at about 0.5 mA and about 1.9 mm at 3.0 mA, each at its own optimum G2 voltage (US 6,605,898). Exponent about 0.42. [S][D]
  - Typical bipotential colour-TV gun at 4.0 mA: centre spot "typically in the 100-140 mil range" (2.5 to 3.6 mm) with the edge at 10 % of peak current density (US 4,529,910). For a Gaussian that is about 1.4 to 2.0 mm FWHM. [S][D]
- Apparent exponents of d against I from colour tubes span 0.22 to 0.42, consistent with the monochrome 0.29. Because the floor d0 lowers the apparent exponent, n in the model below is at least these values; the sourced span supports n = 0.2 to 0.5. [D]
- Model: d² = d0² + k·I^(2n), n between 0.2 and 0.5 (was 0.3 to 0.5 before T-63), with a non-zero floor d0 set by the gun optics. [E][D]
- With I ∝ V^γ, width versus video level goes roughly as V^0.7 to V^1.2 above the floor. [D]
- Black to full white changes width by a factor of about 1.5 to 2.5 (up to 3 in poor consumer tubes). [E]
- ABL and amplifier limits cap the growth. [S]

### Position dependence of the spot

- A single static focus voltage cannot focus the whole screen. Dynamic focus adds parabolic H and V waveforms (a few hundred volts to about 1 kV on a 5 to 8 kV focus). [S][E]
- Oblique landing stretches the spot by 1/cos θ (about 1.2 to 1.5 at corners of 90 to 110° tubes). [D]
- Self-converging in-line yokes stretch corner spots horizontally and add a vertical haze. Dynamic astigmatism-and-focus or quadrupole guns correct this on better sets (PVM/BVM, high-end monitors, late TVs). [S][E]
- Lower anode voltage also defocuses the beam. [S]
- Edge-to-centre ratio is about 1.5 to 2.5 times without dynamic focus and 1.1 to 1.4 times with it. [E]

### Scanline structure

- Each scanline is a horizontal trace of the spot, so its vertical profile is the spot's y-profile centred on the line.
- Line pitch P = visible height / number of active lines (assuming the raster fills the visible area).
- Flat-field ripple of equal-brightness Gaussian lines (valley at midway between lines relative to line peak, and Michelson modulation):

| σ/P | FWHM/P | valley/peak | Michelson modulation |
|---|---|---|---|
| 0.15 | 0.35 | 0.008 | 98 % |
| 0.20 | 0.47 | 0.09 | 84 % |
| 0.25 | 0.59 | 0.27 | 57 % |
| 0.30 | 0.71 | 0.50 | 34 % |
| 0.35 | 0.82 | 0.70 | 18 % |
| 0.40 | 0.94 | 0.84 | 8.5 % |
| 0.45 | 1.06 | 0.93 | about 3.5 % |
| 0.50 | 1.18 | 0.97 | 1.4 % |
| 0.60 | 1.41 | 0.997 | below 1 % |

- Rule: scanlines vanish when FWHM ≥ line pitch (σ/P ≥ about 0.45). [D]
- For small ripple, modulation ≈ 2·exp(−2π²σ²/P²). [D]
- A 480i TV is designed so the two interleaved fields give a nearly flat field (σ/P(480) about 0.5). The same spot at 240p has σ/P(240) of about 0.25, giving about 57 % modulation. That is why 240p shows dark gaps and 480i does not. [D]
- Spot growth with current means bright lines are fat and fill the gaps, dark lines are thin and open them. [S]
- Small tubes show weaker scanlines than large ones for the same gun, because the line pitch is smaller. [D]
- Example pitches at 240 lines: 14 in 0.83 mm, 20 in 1.21 mm, 26 in viewable 1.65 mm. [D]
- Raster lines are tilted: vertical deflection is a ramp, so each line descends by one pitch over its width (about 1/300 rad at 240p). Negligible visually but real. [D]
- Kell factor: effective vertical resolution is about 0.7 times the line count. [E]

### Line modes and interlace

| Mode | Active lines | Field rate | Notes |
|---|---|---|---|
| 240p (NES/SNES/Genesis/Saturn/PS1 2D, 15 kHz arcade) | 224 to 256 visible, 262 or 263 total | about 60.1 Hz progressive | Every field lands on the same lines, so gaps are never filled |
| 288p (PAL) | 288 of 312 | 50.08 Hz | |
| 480i / 576i | 480 / 576 | 59.94 / 50 fields/s | Fields are offset by half a line. Phosphor has decayed before the next field, so fields blend only in the eye (flicker and line twitter) |
| 480p | 480 | 59.94 Hz | 31.469 kHz line rate |
| VGA 200-line modes (320×200) | 400, double-scanned | 70.09 Hz | Each source row is drawn twice, giving fine paired scanlines, not bold 15 kHz ones |
| PC modes | 480 to 1200 | 60 to 160 Hz | Scanlines hardly visible at high resolution |

### Beam and mask interaction (model)

- The beam spot covers several mask apertures: FWHM is typically 1.5 to 3 triad pitches on monitors and TVs, so a single spot illuminates 2 to 10 triads. [S][E]
- The mask samples the beam energy. Each phosphor element of colour c lights in proportion to beam c at its aperture:

```
L_c(x, y) = Phi_c(x, y) * sum over apertures of [ integral over aperture of j_c(x', y') dA ]
```

  where Phi_c is the element shape (defined by the black matrix) and j_c the current density of gun c. [D]
- A practical form is L_c(x, y) ≈ M_c(x, y) · B_c(x, y), with B_c the continuous beam image (scanlines in y, bandwidth-limited signal in x) and M_c the emitter mask. [D]
- The approximation fails when the spot is smaller than an element (high-resolution monitor at low current): then elements light non-uniformly or partially, visible as uneven dot brightness on single-pixel detail.
- The three beams pass through the same aperture and land p/3 apart. The resulting colour-dependent shift is at most p/3 and is normally absorbed in convergence adjustment. [S][D]
- With an aperture grille, vertical detail is limited only by the spot. With a dot or slot mask, the vertical mask period also samples the scanlines, softens them and can cause scan moiré. [E]
- A model that samples beam energy at phosphor positions reproduces moiré and chromatic fringing automatically, but only if the host sampling is fine enough.

### Horizontal versus vertical sharpness

- Horizontally: signal at the pixel clock, convolved with amplifier response and spot x-profile, then sampled by the mask.
- Vertically: discrete scanlines blurred only by spot y-profile.
- The two are independent and generally unequal. On TVs horizontal blur dominates (amplifier contributes about as much as the spot). On PC monitors the amplifier is negligible at low resolutions. [S][D]
- Garcia-Pérez and Peli measured more than 5 % mean-luminance loss for high-contrast vertical gratings (bars across scan lines) and none for horizontal ones. [S]

### Scan velocity modulation

- Small auxiliary coil modulates horizontal beam speed at video transitions (Sony, Panasonic, Philips consumer TVs of the 1990s). [E]
- It sharpens horizontal edges, with a slight bright/dark outline of about 0.1 to 0.3 mm. PVM/BVM usually have it off.

## Phosphors

### Composition and emission

| Colour | Compound | Emission | EIA decay class | Tag |
|---|---|---|---|---|
| Red (P22R) | Y₂O₂S:Eu, with Fe₂O₃ pigment | Narrow lines, main line about 626 nm (satellites about 612/620 and 705 nm) | medium | [S][K] |
| Green (P22G) | ZnS:Cu,Al or ZnS:Cu,Au,Al (older (Zn,Cd)S:Cu,Al) | Broad band, peak about 530 to 535 nm | medium short | [S][K] |
| Blue (P22B) | ZnS:Ag (with Cl or Al co-activator), CoAl₂O₄ pigment | Broad band, peak about 450 nm | medium short | [S][K] |

- P22 is the registered designation of the three-phosphor colour TV set. [S]
- Wavelengths differ between compilations (red 611 or 626 or 643 nm). Use chromaticity xy, not wavelengths.
- Pigment-coated grains reduce ambient reflection. Late tubes used colour-filter layers (Toshiba Microfilter). [K][E]
- Efficiency is roughly 20 % for ZnS:Ag and ZnS:Cu,Al and 13 % for Y₂O₂S:Eu. [E]
- A tube holds about 7 g of phosphor. [S]
- EIA persistence classes (time to 10 %): very short under 1 µs, short 1 to 10 µs, medium short 10 µs to 1 ms, medium 1 to 100 ms, long 0.1 to 1 s. [K] Class boundaries differ between sources.

### Chromaticity (CIE 1931 xy)

Standards:

| Set | R | G | B | White | Tag |
|---|---|---|---|---|---|
| NTSC 1953 | 0.670, 0.330 | 0.210, 0.710 | 0.140, 0.080 | Illuminant C (0.3101, 0.3162) | [S] |
| SMPTE-C (RP 145 / 170M) | 0.630, 0.340 | 0.310, 0.595 | 0.155, 0.070 | D65 | [S] |
| EBU Tech 3213 | 0.640, 0.330 | 0.290, 0.600 | 0.150, 0.060 | D65 | [S] |
| Rec.709 / sRGB | 0.640, 0.330 | 0.300, 0.600 | 0.150, 0.060 | D65 | [S] |
| ARIB TR-B9 "Japan specific" | 0.618, 0.350 | 0.280, 0.605 | 0.152, 0.063 | D93 | [S] |
| Measured P22 (SMPTE J. 1982) | 0.635, 0.340 | 0.305, 0.595 | 0.155, 0.070 | n/a | [S] |

- No real tube reached NTSC 1953 primaries after the 1960s. [K]
- Sony BVM-D monitors hold SMPTE-C or EBU coordinates to ±0.005. [S]
- Some tables list EBU green as (0.30, 0.60), which is the Rec.709 value. EBU Tech 3213 is (0.29, 0.60). [S]

Measured or vendor-stated tubes:

| Tube or set | R | G | B | Note |
|---|---|---|---|---|
| Trinitron GDM-17SE1 (Color.org) | 0.621, 0.340 | 0.281, 0.606 | 0.152, 0.067 | [S] |
| Trinitron GDM-17SE1 (Bohnsack 1994) | 0.6233, 0.339 | 0.2838, 0.5895 | 0.1519, 0.0669 | [S] |
| Sony PVM-20M2U | 0.630, 0.345 | 0.285, 0.605 | 0.150, 0.065 | [S] |
| Sony PVM-20L2MDU | 0.625, 0.345 | 0.280, 0.605 | 0.150, 0.065 | [S] |
| Apple RGB (13 in Trinitron) | 0.625, 0.340 | 0.280, 0.595 | 0.155, 0.070 | [K] widely repeated, not verified against a Sony document |
| Hitachi CM2198 | 0.624, 0.339 | 0.285, 0.604 | 0.150, 0.065 | ±0.02 [S] |
| NEC MultiSync C400 | 0.610, 0.350 | 0.307, 0.595 | 0.150, 0.065 | [S] |
| KDS VS19 | 0.625, 0.340 | 0.285, 0.605 | 0.150, 0.065 | [S] |
| Dell (most models) | 0.625, 0.340 | 0.275, 0.605 | 0.150, 0.065 | [S] |
| Mean of 15 monitors (Golz and MacLeod 2003) | 0.620, 0.342 | 0.288, 0.597 | 0.153, 0.070 | [S] |
| "P22 average", Japanese consumer | 0.625, 0.350 | 0.280, 0.605 | 0.152, 0.062 | [S] |
| Panasonic CT-36D30B (2000, US) | 0.6512, 0.3272 | 0.3117, 0.6077 | 0.1503, 0.0666 | hobbyist measurement [S] |
| RCA ColorTrak E13169GM | 0.6235, 0.3440 | 0.3123, 0.5903 | 0.1536, 0.0645 | hobbyist measurement [S] |
| Toshiba Blackstripe CF2005 (1985) | 0.642, 0.339 | 0.301, 0.601 | 0.144, 0.071 | aged unit [S] |
| ColorMatch RGB (Radius PressView, P22) | 0.630, 0.340 | 0.295, 0.605 | 0.150, 0.075 | D50, γ 1.8 [K] |

- Practical reading: PC-monitor P22 sets cluster at R (0.62 to 0.63, 0.34), G (0.28 to 0.29, 0.60), B (0.15, 0.065). They are less saturated in red than sRGB and slightly more yellow-green in green. [S]
- Real tubes deviate from nominal by about ±0.005 to 0.01 (up to 0.02 between models).
- All real CRTs cover roughly 70 to 100 % of the Rec.709 area. A 3×3 matrix and selectable primaries are adequate; full spectral modelling is not needed for an RGB shader. [K][E]

### White point

| Name | x | y | Notes |
|---|---|---|---|
| D65 | 0.3127 | 0.3290 | Studio standard in the US and Europe, and PC sRGB [S] |
| Illuminant C | 0.3101 | 0.3162 | NTSC 1953, used until SMPTE 170M (1987) [S][K] |
| 9300 K + 27 MPCD | 0.281 | 0.311 | Japanese TV practice [S] |
| 9300 K + 8 MPCD (CIE 1960 MPCD), usually called D93 | 0.2831 | 0.2971 | [S][D] (daylight-locus formula) |
| 9300 K + 8 MPCD (Judd MPCD) | 0.2838 | 0.2984 | [S] |
| Service-manual 9300 K targets (Samsung, Nokia, Philips, Sony) | 0.283 to 0.285 | 0.297 to 0.298 | ±0.02 [S] |
| Trinitron GDM-17SE1 at 9300 K | 0.2836 | 0.2963 | [S] |
| NEC C400 at 9300 K | 0.280 | 0.315 | [S] |
| Sony PVM-20L5 at D65 | 0.3131 | 0.3294 | about 6480 K [S] |
| RCA ColorTrak (1989) | 0.301 | 0.308 | about 7500 to 8000 K [S] |

- 1 MPCD = 0.004 Δu′v′. [S]
- PC monitors shipped at 9300 K by default, with 6500 K and 5000 K presets. [E]
- Japan kept 9300 K for consumer TVs after the US moved to D65. US consumer sets were typically 7000 to 9300 K. One late Panasonic set measured near D65 in "Warm", near 9300 K in "Normal" and near 14000 K in "Cool". [S]
- Colour temperature is set by per-gun gain (drive) and cutoff (bias). A white-point change is a per-channel gain in linear light, not a chromatic adaptation. [E]
- White balance drifts with level if cutoffs are not matched (grey-scale tracking). A Sony service target lists (0.285, 0.298) at 80 IRE against (0.277, 0.287) at 20 IRE. [S]

### Decay and persistence

Published values conflict by up to about 100 times (see "Resolved conflicts" for the reconciliation).

| Phosphor | Time to 10 % | Time to 1 % | Residual at 8 ms | Residual at 16 ms |
|---|---|---|---|---|
| Red Y₂O₂S:Eu | 0.85 ms (AST/APACE table) or 1.5 ms (Sherr 1993) | 1.9 ms (Sony patent) | 0.25 % | 0 % |
| Green ZnS:Cu,Al | 35 µs (AST/APACE) or 6 ms (Sherr) | 7.5 ms | 0.95 % | 0.6 % |
| Blue ZnS:Ag | 80 µs (AST/APACE) or 4.8 ms (Sherr) | 7.0 ms | 0.9 % | 0.5 % |

- The "Time to 1 %" and residual columns come from US 5,825,124 (Sony), Table 1. The patent gives the residuals relative to "the maximum afterglow intensity peak" after excitation "by an electron beam" and states no pulse length, beam current or raster. They are therefore not fractions of an impulse-response peak (T-63). [S]
- Its τ column is labelled "τ (1/100)" in the table but "reduced to 1/000" in the text, so the threshold of the "Time to 1 %" column is uncertain. [S]
- The same patent adds 2 to 10 ppm Ni ("killer") to the sulfides: the 8 ms residual falls from 0.966 % to 0.220 % (2 ppm) and 0.072 % (10 ppm) for green, and from 0.885 % to 0.295 % and 0.052 % for blue, at about 10 to 14 % luminance loss (2 ppm). Tail strength therefore varies more than tenfold between phosphor batches. [S]
- Kuhn (2002) measured a P22 VGA monitor (640×480 at 85 Hz, photomultiplier, 5 GHz sampling over 40 µs for a single pixel, 125 MHz over 2 ms for a 320-pixel line) and fitted sums of exponentials plus a power law per phosphor (table below). Times to a fraction of the impulse peak derived from that fit: green 12 µs to 10 %, 0.14 ms to 1 %, 3.5 ms to 0.03 %; blue 0.45 µs, 21 µs, 0.61 ms; red 32 µs, 0.89 ms, 2.4 ms. [S][D]
- A red channel on an Iiyama HM204DT (Diamondtron) peaked 0.18 ms after excitation and decayed to zero within 2 ms (Elze). [S]
- Wolf and Deubel (1997, "P31 phosphor persistence at photopic mean luminance level", Spatial Vision 10(4), 323-333) measured a P31 monochrome phosphor (HP 1300A), not P22, so it does not bound the colour-tube trail. Full text read in the PHY-19 research pass: a bar on a 20 cd/m² refreshed background at 100 or 1000 frames/s left a residual contrast below 5 % after 10 ms, below 2.5 % after 30 ms and 1.4 % after 50 ms. That is the decay of the modulation of a repetitively excited area, not an impulse response. Correction (2026-11-21): this file earlier credited the paper with "about 4 % after 20 ms"; no such figure appears in it. Frame-sequential stereo crosstalk reached 8.5 % on a P22 monitor at 122.6 Hz. [S]
- A residue of 0.5 % linear is about sRGB code 16 of 255, so a white object moving over black leaves a visible greenish-cyan trail for a frame or two. [D]
- A tail can only be ignored once it falls below a linear level of about 0.0003. A power-law tail can stay above that for up to a second. [S]
- Red is slower to reach 10 %. Green and blue are slower below 1 %, so both statements about "which colour trails" are true at different levels. [S][D]

Recommended model per colour (impulse response, sum of exponentials plus one power law):

```
I(t) = sum_i A_i * exp(-t / tau_i) + B * (1 + t / t0) ^ (-n)      normalised so that I(0) = 1
```

Constants from Kuhn (2002), equations 8 to 10, measured on one P22 monitor (exact P22 variant unknown), rewritten in this form (Kuhn's terms A·exp(−2π f t) give tau = 1/(2π f); his power law a·((t + α)/1 s)^(−β) gives t0 = α, n = β, B = a·α^(−β)), then divided by I(0):

| Channel | Exponential terms A_i (tau_i) | Power-law term B, t0, n | Tag |
|---|---|---|---|
| Red | 0.0741 (442 µs), 0.0324 (99.5 µs), 0.0370 (19.9 µs), 0.0417 (6.37 µs), 0.2778 (0.227 µs), 0.5370 (0.0227 µs) | none ("decays purely exponentially") | [S] fit by Kuhn, [D] normalisation |
| Green ZnS:Cu,Al | 0.1042 (1.061 µs), 0.2815 (0.227 µs), 0.2534 (0.0318 µs) | B = 0.3609, t0 = 5.5 µs, n = 1.1 | [S] fit by Kuhn, [D] normalisation |
| Blue ZnS:Ag | 0.0323 (1.592 µs), 0.4309 (0.145 µs), 0.4740 (0.0398 µs) | B = 0.0627, t0 = 5 µs, n = 1.11 | [S] fit by Kuhn, [D] normalisation |

Frame-energy ratios of these constants for a pixel excited once (E_k = integral of I over frame k, frame 0 = [0, T]): [D]

| Channel | Rate | E1/E0 | E2/E0 | E3/E0 |
|---|---|---|---|---|
| Green | 60 Hz | 5.36 % | 2.97 % | 2.03 % |
| Blue | 60 Hz | 4.72 % | 2.60 % | 1.78 % |
| Red | 60 Hz | below 1e-6 | 0 | 0 |
| Green / blue | 50 Hz | 5.19 % / 4.57 % | | |
| Green / blue | 120 Hz | 6.09 % / 5.37 % | | |

- Kuhn notes that n must exceed 1 for the emitted energy to be finite, and that his measurement "was not designed to estimate" n. His records end at 2 ms, so everything the table says beyond about 2 ms (including E1 to E3) is an extrapolation of the fitted power law. [S]
- With n = 1.1, about 21 % (green) and 19 % (blue) of the light emitted within 1 s of an excitation comes after the first 60 Hz frame. A static image therefore gets about a fifth of its green and blue luminance from earlier frames' tails, which the frame-feedback normalisation must absorb. [D]
- Cross-check with the patent residuals: for a rectangular excitation of length D, the residual relative to the level at the end of the excitation, from Kuhn's green fit, is 0.23 % / 0.11 % at 8 / 16 ms for D = 0.1 ms and 1.36 % / 0.66 % for D = 1 ms (blue 1.17 % / 0.56 %). The patent's 0.95 % / 0.6 % (green) and 0.9 % / 0.5 % (blue) therefore fit an excitation of somewhat under 1 ms, but not an impulse (0.012 % / 0.006 % of the impulse peak for green). [D]
- Between 8 and 16 ms the patent residuals fall by only 0.56 to 0.63 times (a local power law of about t^−0.7 to t^−0.85), slower than Kuhn's t^−1.1. If the patent slope applies, E1/E0 is somewhat higher than the table. [D]
- Cross-check with frame-sequential stereo: at 120 Hz the fit gives about 6 % green trail energy; the 8.5 % crosstalk measured on a P22 monitor at 122.6 Hz also includes shutter leakage and timing, so its phosphor part is below 8.5 %. Same order of magnitude, while the superseded fit below gives 31 %. [D]
- Superseded (T-63): the earlier fit A = 1, tau = 0.03 to 0.1 ms, B ≈ 0.046, t0 = 1 ms, n ≈ 0.72 anchored the patent residuals to an impulse peak. That makes the tail about 75 times too strong relative to the fast part (0.90 % against 0.012 % of the peak at 8 ms), gives E1/E0 = 29.7 % at 60 Hz, and has n < 1 (infinite total energy). Do not use it. [D]
- With a red 1/e time of 0.44 ms (slowest red term), the visibly bright band (above 10 % of that term) is about 1 ms tall, about 16 lines of a 262-line field. The fast sulfide components are confined to a fraction of one line behind the beam. [D]
- At 60 Hz, red has emitted 91 % of its energy within 1 ms and practically all within 2 ms. Green and blue emit about half their first-frame energy within 0.1 ms and about three quarters within 1 ms; the rest is the power-law tail. [D]

### Phosphor types

Research pass 2026-11-21 (QUESTIONS PHY-19), JEDEC/EIA "P" phosphors for direct-view monochrome tubes. "10 %" and "1 %" persistence follow the JEDEC definition: time for the light to fall to that fraction of its brightness when a steady excitation is removed (a step-off response of one spot, not Kuhn's impulse response; the two coincide only for an exponential). Persistence also depends on excitation duration and level and varies between tube makers (Flynt 1989, cited by Wolf and Deubel); the registered values are nominal. Reliability: A primary (RCA 1961 [R], HP AN-115 1970 [H], Phosphor Technology catalogue [PT], Philips patents [Ph], Wolf and Deubel [WD]); B vendor tables without conditions; C secondary.

| Type | Composition | CIE 1931 x, y | Time to 10 % | Time to 1 % | Source |
|---|---|---|---|---|---|
| P1 | Zn₂SiO₄:Mn (willemite) | 0.218, 0.712 [R][H]; 0.208, 0.704 [PT] | 24.5 ms [R], 24 ms [H] | 48 ms [H] | A [S] |
| P4 sulfide (direct-view B&W TV) | ZnS:Ag + (Zn,Cd)S:Ag | 0.270, 0.300 [R][H] | 60 µs [R][H] | 470 µs [H] | A [S] |
| P31 | ZnS:Cu | 0.245, 0.523 [R]; 0.287, 0.521 [PT] (disagree by 0.04 in x, unresolved) | 38 µs [R][H]; EIA TEP116-B via [WD] | 250 µs [H]; EIA via [WD] | A [S] |
| P39 | Zn₂SiO₄:Mn,As | 0.223, 0.698 [H]; 0.195, 0.697 [PT] | 150 ms [H] | not found (curve concave on semilog, not exponential) | A [S] |
| Philips yellow data-display blend (no P number) | 15 % Zn₂SiO₄:Mn,As + 85 % CdSiO₃:Mn,As | 0.480, 0.490 (example tubes) [Ph, US 4,405,880] | at least 34 ms | not found | A [S] |
| P7 (cascade) | ZnS:Ag on (Zn,Cd)S:Cu | blue 0.151, 0.032; yellow 0.357, 0.537 [R][H] | yellow 400 ms [R], 0.3 s [H] | 3 s [H] | A [S] |
| P38 | (Zn,Mg)F₂:Mn | 0.561, 0.437 [H] (last digit uncertain); 0.541, 0.450 [PT] | 1 s [H] | not found | A [S] |
| P43 | Gd₂O₂S:Tb | 0.337, 0.561 [PT] | 1 ms (90 to 10 %), Exosens | +1.6 ms (10 to 1 %) | B [S] |
| P45 | Y₂O₂S:Tb | 0.263, 0.342 [PT] | 1.5 ms, AST/APACE | not found | B [S] |
| P3, P134 | (P3: Zn-Be silicate, C only) | not found | P3 13 ms (C only) | not found | none usable |

Shipped in the selector (`phosphor_type`, data rows `crt_model::phosphor_types`) and how each decay is modelled [D]:

- P4: a pure power law (1 + t / t0)^-n through both registered points: t0 = t10² / (t1 - 2 t10) = 10.29 µs, n = ln 10 / ln(1 + t10 / t0) = 1.198. P31: the same, t0 = 8.30 µs, n = 1.339 (n > 1: finite energy). The power-law form for the sulfides follows Dyall 1948 (P4 mixed exponential and power law; via Jankowiak, C) and Kuhn's power-law tail for the P22 ZnS:Cu green. At 60 Hz the trail is small (P4 3.9 %, P31 1.7 % of a frame in the next one).
- P1: one exponential, tau = 24 ms / ln 10 = 10.4 ms; the 48 ms 1 % point lies on it. Frame ratio at 60 Hz exp(-16.67 / 10.4) = 0.20.
- P39: one exponential through the one point, tau = 150 ms / ln 10 = 65.1 ms. A one-point approximation: the registered curve is not exponential and its 1 % time is unknown. Frame ratio at 60 Hz 0.77.
- Philips yellow blend: one exponential through the lower bound, tau = 34 ms / ln 10 = 14.8 ms (the real decay is at least this long). Named as the patent describes it, never P3 or P134.
- In every case the registered step-off times are used as the impulse response: exact for an exponential only.
- Held back: P7 and P38 (3 s to 1 % and 1 s to 10 % exceed the 1 s window of the frame-feedback fit), P3 and P134 (no chromaticity, or no data at all; "P134 amber" rests on one Usenet post), P43 and P45 (B-class decay only).

A monochrome tube has one gun and a continuous coating with no triads (Wikipedia "Monochrome monitor", C). The model drives it with the BT.601 luma Y' = 0.299 R' + 0.587 G' + 0.114 B' of the gamma-encoded source, the signal a composite or monochrome-video monitor receives [K] (with a source colour target on, the target's colour comes first and the luma is that of the target's output); MDA-style TTL monitors had digital intensity levels instead, which the model does not reproduce. P1 and P39 (y about 0.70), P31 at 0.245, 0.523 and the yellow blend lie outside the sRGB triangle (the B-G edge at y = 0.523 is at x = 0.279, the R-G edge at y = 0.49 at x = 0.438) [D]; an SDR output cannot show them exactly.

Why such slow phosphors existed [S]: the IBM 5151 (P39, 50 Hz; Wikipedia, C) traded flicker for smearing; Philips designed 28 to 34 ms data-display phosphors for 20 to 40 Hz refresh [Ph]; AN-115 lists P39 for "medium frame rate displays and radar" [H]. At 60 Hz the smear of P1 and P39 is therefore authentic. What not to claim: an accurate emulation of a given monitor (real tubes vary), a measured P39 afterglow beyond the 10 % point, or a visibility of the tail (a dim-room observer sees a P31 tail for hundreds of ms while a photometer reads it gone within about 2 ms; Di Lollo et al. 1994).

### Saturation and ageing

- Phosphor output is close to linear in beam current at direct-view densities. The gamma is in the gun, not the phosphor. [S]
- Sulfides (blue ZnS:Ag, green ZnS:Cu,Al) saturate at high current density. Oxysulfide red is more linear. Extreme highlights shift toward yellow-red, by a few percent or a few hundred K in direct-view tubes and much more in projection tubes. [S][E]
- Peak instantaneous current density is large because the average is delivered in a short flash (see "Temporal behaviour"). Per-channel soft-clip on instantaneous drive before the mask, stronger on blue, is the suggested shader hook. [E]
- Ageing: one datasheet allows 15 % luminance loss after 20,000 h at 350 cd/m². Blue usually degrades fastest. Static images burn in. [S][K]
- Wear curve: cathodoluminescent phosphors lose efficiency with the deposited charge N per area as I / I0 = 1 / (1 + C N) (Pfahnl's coulombic ageing law, from memory, no reference checked in this research) [K]. It decelerates (half the efficiency at N = 1 / C), so burn-in saturates. The model calibrates C on the datasheet point above, per colour with estimated rate ratios (red 0.5, green 1, blue 2) [E]; no measured per-phosphor curve was found. Cathode wear (emission loss, cutoff drift, larger spot) has no measured curve here either [E].
- Backscattered electrons (coefficient about 0.25) bounce under the mask and can excite neighbouring phosphors, desaturating small saturated features by a few percent. [E]

## Electro-optical transfer, luminance, ABL and glare

### Gun law

- Beam current I = k·(V_drive)^γ above cutoff, where V_drive is the voltage above cutoff. [S]
- The exponent comes from the triode characteristic of the gun, not the phosphor. An ideal diode would give 1.5; real guns give 2.2 to 3.0. [K]
- Quoted display gammas: 2.5 (Poynton, Apple), 2.4 (BT.1886), 2.35 to 2.55 (Wikipedia, BBC), 2 to 3 in the vision literature. The spread is largely a black-level convention issue. [S][K]
- Cutoff differs per gun (spread of about ±10 to 20 V on a 100 to 150 V drive range), which causes dark colour tint (fixed by bias adjustment). [E]

**Gain-offset-gamma (GOG) model** (Berns, Motta and Gorzynski 1993): [K]

```
L_c(d) = L_max,c * ( k_g,c * d/(2^N - 1) + k_o,c )^gamma_c   if (k_g d + k_o) >= 0, else 0
XYZ    = M_3x3 (phosphor primaries) * [L_r, L_g, L_b] + XYZ_flare
```

**ITU-R BT.1886** (2011): [S]

```
L = a * max(V + b, 0)^2.4
a = (Lw^(1/2.4) - Lb^(1/2.4))^2.4
b = Lb^(1/2.4) / (Lw^(1/2.4) - Lb^(1/2.4))
```

**BT.1886 Annex 1** (closer CRT match, two exponents): [S]

```
L = k * (V + b)^2.6                                   for V >= 0.35
L = k * (0.35 + b)^(2.6 - 3.0) * (V + b)^3.0          for V <  0.35
k = Lw / (1 + b)^2.6
```

- The Grade shader uses default 2.50, range 2.30 to 2.60, noting that effective gamma runs from 2.40 at black level 0.1 to 2.60 at zero black level. [S]
- sRGB has a linear toe and averages about 2.2, so decoding CRT-era content as sRGB lifts shadows compared with a real tube. A real CRT has zero slope at cutoff (no linear toe). Example: sRGB level 20/255 gives 0.007 linear, pure 2.4 gives 0.00222, pure 2.2 gives 0.00370 (corrected by calculation, checked by the `crt-model` tests; an earlier version said 0.0049 and 0.0057). [D]
- Consoles authored for CRT gamma, PC content for sRGB. The source gamma must be a user choice; there is no single correct default. [E]

### What the user controls do physically

| Control | Physical action | Model effect |
|---|---|---|
| Brightness / black level | DC bias at cathode relative to cutoff | k_o (offset before the power law). Changes effective gamma near black, enlarges spot at dark levels when raised |
| Contrast / picture | Video amplifier gain | k_g. Raises peak current, spot growth, blooming, ABL activity |
| Screen (G2) | Sets cutoff voltage of all guns | Global black level; slightly affects spot |
| Focus (G3/G4) | Main lens voltage | Spot σ; dynamic focus adds parabolas |
| RGB drive / cutoff (service menu) | Per-gun gain / bias | White point at highlights / near black |
| Sharpness (consumer) | Peaking filter in the video path, about +3 to 8 dB around 3 to 4 MHz | Ringing/halo on horizontal edges only, 5 to 20 % overshoot |
| Colour / tint | Chroma decoder (composite/YC only) | Not relevant to RGB input |

- Setup pedestal (7.5 IRE NTSC-US, 0 IRE NTSC-J) is irrelevant for RGB PC capture except that shader black should sit at cutoff.
- Raising brightness above the correct point adds a pedestal (greys blacks, lowers effective gamma). Lowering it crushes shadows and raises effective gamma. [S]

### Luminance and contrast

| Quantity | Value | Tag |
|---|---|---|
| Broadcast reference white | 100 cd/m² (70 for a 32 in BVM-D); BVM-1911 103 cd/m² | [S] |
| Broadcast practice (EBU/SMPTE) | 80 to 120 cd/m², dark surround | [K] |
| PC monitor | about 150 typical, about 200 max; calibrated use 80 to 120 | [S][E] |
| Medical monochrome tubes | 240 to 514 cd/m² | [S] |
| Consumer TV, full white | about 80 to 200 cd/m² (ABL-limited) | [E] |
| Consumer TV, small-area peak | about 250 to 600+ cd/m² | [E] |
| Black level, dark room, brightness set correctly | essentially zero; practical floor 0.01 to 0.05 cd/m² from reflection and halation | [S][E] |
| Sequential (full on/off) contrast | above 10,000:1 (15,320:1 measured on a Toshiba 34HF84) | [S] |
| ANSI checkerboard contrast | 100 to 150:1 (135:1 measured on the same set); medical CRT about 100, below 25 with ambient light | [S] |
| Older text figure for intra-image contrast | 20:1 to 40:1 | [S] |

### ABL, blooming and EHT regulation

- **ABL** senses average anode current at the flyback and reduces contrast (sometimes brightness) above a limit. It responds to the frame average with a time constant of several fields up to about a second. [S][E]
- Threshold is about 0.5 to 2 mA average on TVs and 0.3 to 1 mA on monitors. [E]
- Sourced TV values (T-63): RCA/Thomson average limiter activated at 1.4 mA, lowered to about 1 mA or 0.7 mA in a peak-responsive mode, with peak current about 25 % above average (US 5,313,294). [S]
- Timing: limiting starts after about 0.1 s and reaches steady state about 0.5 s after the threshold is crossed (US 4,330,792, one RCA example); a prior-art average filter has a time constant of about 5 field periods (Sutherland US 4,689,668, as described in EP 0 394 003). [S]
- No source found for the contrast reduction at full-field white; the 30 to 60 % figure stays an estimate.
- Full-field white is typically 30 to 60 % of a small white window on consumer sets. PVM/BVM and good monitors have very mild ABL. [E]
- Model: global contrast gain g = 1/(1 + k·max(0, APL_filtered − I_th)) or clipped linear, driven by a low-pass of mean beam current. [E]
- **Raster blooming ("breathing"):** EHT sags with beam current. Raster size scales as 1/√V, so a 5 % sag enlarges the picture by about 2.5 %. [D]
- Regulation figures: 3.0 % on a Sony BVM-8045QD; unregulated consumer flyback supplies sag 5 to 10 % at full current; consumer TVs change raster size by about 1 to 5 % between black and white; broadcast spec keeps raster size within 1 % of picture height from 10 to 90 % APL. [S][E]
- Lower anode voltage also defocuses the beam.
- Unregulated B+ makes width follow mains (±10 % mains gives about ±5 % width). [E]
- "Blooming" has two meanings, and both are real: the whole raster breathing with average picture level, and the local spot enlarging at high current.
- **DC restoration:** consumer TVs often do not fully restore DC, so black level drifts with content (2 to 5 % streaks beside bright objects). [S][E]

### Uniformity

- Centre-to-corner luminance: 73 % on a Barco CDCT 6551, a 13 % drop on a Sony 17se, "up to 30 %" in general, 70 to 90 % typical. A 21 in tube datasheet specifies corners between 80 % and 110 % of centre. [S]
- Causes: longer path (lower current density), oblique incidence reducing effective aperture, thicker tinted glass at the edges, mask grading. [E]
- Colour uniformity: white-point drift of 100 to 500 K across the screen, with corner tints. [E]
- Linearising from a small patch does not hold for full-field patterns: luminance depends on image size as well as position. [S]

## Glass, halation, glare and reflections

### Faceplate

- Refractive index n about 1.52 (CRT panel glass 1.52 to 1.54). Critical angle θc = asin(1/n) ≈ 41°. [K][D]
- Standard panel transmittances at 546 nm through 10.16 mm of glass: 86, 73, 57, 46, 42 and 30 %. One 21 in tube lists 43 % at screen centre; another 49 ± 3 % for the bulb plus a bonded AR front panel of about 95 %. [S]
- Rough classes: clear 85 to 90 %, grey 70 to 75 %, dark tint 40 to 55 %, extra dark 30 to 40 %. [S][E]
- Centre thickness, sourced from patents (T-63): [S]
  - LG.Philips claims 9 to 11.5 mm for 4:3 tubes of 400 to 500 mm diagonal, 10 to 12.4 mm for 4:3 tubes of 570 to 700 mm, and 11 to 13.4 mm for 16:9 tubes of 650 to 760 mm (US 7,176,611).
  - Toshiba 26 in curved tube: 12.0 mm at the centre, 14.7 to 15.7 mm at the edge centres and corner (US 4,537,322, first example).
  - Mitsubishi flat-outside tubes: 12 mm (41 cm) and 14 mm (50 cm) (US 6,133,686).
  - Sony 28 in flat-face tube: 15 mm at the centre, 18 mm at the periphery (US Re. 39,739).
- So about 9 to 13.5 mm at the centre for 400 to 760 mm diagonals, up to 15 mm on a 28 in flat-face tube. Flat-face edges are thicker (18 mm on the Sony example; 20 to 30 mm at the corners remains an estimate). With tint, edges darken as T_edge = T_centre^(t_edge/t_centre). [S][E][D]
- RCA 1983 colour TV tube datasheets give 84 to 85.5 % light transmittance at the centre of the faceplate (clear glass) for 36 to 68 cm tubes, all with "Medium Short" persistence phosphors. [S]
- Grey glass attenuates emitted light once and reflected ambient light twice, so ambient contrast improves by about 1/T. [S][D]

### Halation (internal reflection ring)

- Light entering the glass beyond θc is totally reflected at the front surface. It returns to the phosphor at radius r = 2·t·tan θc ≈ 1.74 to 1.75 t, forming a ring with a sharp inner edge. A patent states halo diameter is about 3.5 times the faceplate thickness. [S][D]
- For t = 10 to 14 mm the ring radius is about 17 to 25 mm.
- Fraction of Lambertian emission inside the escape cone is sin²θc = 1/n² ≈ 43 %, so about 57 % is trapped at first incidence. [D]
- Inside the ring there is a weaker haze from ordinary Fresnel reflection (4 % at normal incidence).
- The returned light crosses the tinted glass obliquely twice more. Relative to the direct image it is attenuated by about T^2.65. For T = 0.46 that factor is 0.13; for T = 0.86 it is 0.67. Dark glass therefore suppresses halation strongly. [D]
- The black matrix absorbs returning light in proportion to its area fraction.
- Amplitude: estimates range from about 1 to 5 % of the source energy in the halo (Sonnet and Opus agents) to a total stray light of 0.5 to 1 % at the centre of a dark patch inside a bright surround (Fable agent, from measured ANSI contrast). Treat as a tunable of about 0.5 to 5 %. [E]
- Measured total veiling glare (T-63): Flynn and Badano (1999) report a glare ratio (test image with a 1 cm black spot) of 25 for a colour monitor, 89 for a monochrome monitor and 555 for film. Read as surround luminance over spot-centre luminance (the abstract does not define it further), a ratio of 25 means stray light at the centre of the spot of about 4 % of the surround; the 240 of a high-performance medical monitor means about 0.4 %. [S][D]
- These bound all stray-light mechanisms together, halation included. No source separates the halation ring amplitude, so the 0.5 to 5 % range is kept, with 4 % total glare as a sourced upper reference for a colour monitor. [E]
- Shader model: convolve linear emitted luminance with a kernel of annulus (r ≈ 1.74 t, width ≈ t) plus short-range diffusion, applied in linear light. [E]

### Other stray-light mechanisms (1990s medical-display research)

1. Internal reflection in the faceplate glass: local, the most important, coloured. [S]
2. Light leaking from the back of the phosphor layer into the tube and returning: nearly uniform, coloured. [S]
3. Electron scattering between phosphor and mask: short range, excites all colours, desaturates. [S]
4. Electrons backscattered from the mask: long range, desaturating. [S]

- Phosphor-layer scatter gives a near-field glow on the scale of 0.05 to 0.5 mm that partly fills black-matrix gaps. [E]
- Veiling glare measurement: ratio of 240 for a bright surround over a 1 cm dark disc on a high-performance medical monitor with AR coating. [S]
- Electron-based mechanisms (3 and 4) are dwarfed by the optical one; crt-royale's "electron halation" description is not the dominant mechanism. [E]

### Surface coatings and reflections

- Bare glass specular reflectance is 4.3 % at normal incidence (((n−1)/(n+1))²), rising to about 8 to 10 % at 60°. [D]
- Measured specular reflection coefficients: 0.041 for a monochrome monitor, 0.021 for a colour monitor with AR coating; an AGAS panel on a 21 in tube is specified under 2.2 %; multilayer AR gets 0.3 to 1 %. [S][E]
- Etched or silica anti-glare surfaces spread the specular reflection into a haze and slightly blur the image. A conductive anti-static layer (ITO/ATO) is typical. [K]
- Diffuse reflectance of the screen ≈ T² · R_phosphor · open matrix fraction: about 8 % for T = 0.46, R = 0.7 and 55 % open. Unlit-screen reflectance without filter is about 20 to 35 %. [D][E]
- Raised blacks in a lit room are about T² × 15 to 40 % of ambient. [D]
- Curved faceplates reflect room lights as distorted highlights ("curvature reflection" in bezel shaders). Reflections are optional for physical accuracy.

## Signal path

### RGB path (the PC capture use case)

- Rise time ≈ 0.35 / bandwidth (10 to 90 %). A Gaussian low-pass of bandwidth B has a time sigma of about 0.1325 / B. [K][D]
- Slew rate is the primary bandwidth-limiting factor of CRT video amplifiers, so the limit is partly nonlinear and larger for big voltage steps. [S]

| Device class | RGB video bandwidth | Rise time | At 256 to 320 px lines |
|---|---|---|---|
| Cheap consumer TV RGB/SCART | 4 to 8 MHz | 44 to 88 ns [D] | 0.2 to 0.55 px of edge softness [D] |
| PVM-xxM2U class | 8 to 15 MHz | 23 to 44 ns | 0.1 to 0.25 px |
| PVM/BVM high-res | 20 to 30 MHz | 12 to 18 ns | very sharp |
| PC monitor | 100 to 400 MHz | 1 to 3 ns | amplifier not the limit; spot is |

- Consumer row corrected (T-14 follow-up): 0.35 / 8 MHz = 44 ns (58 ns is 6 MHz). Edge softness = rise time over the pixel period of a 52.6 µs active line: 44 / 205 = 0.21 px (256 px, 205 ns per pixel) to 88 / 164 = 0.54 px (320 px, 164 ns per pixel). [D]
- At 1600×1200@85 Hz the pixel clock is 229.5 MHz, so even PC amplifier bandwidth is near its limit. [D]
- **Adjacent-pixel nonlinearity** (Mulligan and Stone 1989; Klein, Hu and Carney 1996; Pelli 1997): the cathode drive amplifier slews differently up and down, so an isolated bright pixel is dimmer than a pixel in a run. [S]
  - A one-pixel vertical line is dimmer than a one-pixel horizontal line, by more than 50 % in bad cases.
  - Fine vertical gratings lose more than 5 % of mean luminance at contrasts above 50 %, even with a 10-pixel period. Horizontal gratings are unaffected.
  - The effect is worst with high contrast, high gamma and low bandwidth.
  - Each pixel's voltage depends on the previous pixel along the scan (five-parameter model by Hu and Klein). The smear trails to the right of an edge.
  - Model: a short asymmetric horizontal IIR/FIR in the voltage domain, before the gamma power law.
- Peaking and cable mismatch cause 5 to 20 % overshoot after sharp edges, and cable reflections cause a faint ghost a few pixels to the right. [E]
- The source is effectively zero-order-hold (a DAC box per pixel) then low-passed. Horizontally there is no pixel grid on the tube, only stripes or dots, and pixel boundaries fall at arbitrary mask phases.

### Pixel aspect ratio (important for 1:1 captures)

- The tube stretches the raster to the cabinet aspect (4:3) regardless of pixel count. [E]
- NES/SNES 256-wide at 5.369 MHz: PAR 8:7. [D]
- Genesis H40 at 6.711 MHz: PAR 32:35. H32 is 8:7. [D]
- NTSC square-pixel rate is 12.2727 MHz (480 lines) or 6.136 MHz (240 lines). PAL square-pixel rate is 14.75 MHz (576) or 7.375 MHz (288). [D]
- VGA 320×200 and 640×400 at 70 Hz display with PAR 5:6 (1.2:1 vertical stretch) on 4:3. 640×480 is square. [D]
- A captured 1:1 frame therefore needs a user-selectable display aspect or PAR to look like the original on a tube.

### Composite, S-Video, component (brief)

| Artefact | Cause | Relevant for PC RGB capture? |
|---|---|---|
| Dot crawl, cross-colour | Luma/chroma separation errors at the 3.579545 MHz NTSC (4.43361875 MHz PAL) subcarrier | Only as an optional authenticity pre-stage |
| Chroma bleed | Chroma bandwidth about 0.5 to 1.3 MHz | No (RGB has full chroma bandwidth) |
| NTSC "artifact colours" (Apple II, CGA composite, Genesis dither) | Pixel clock near subcarrier | Only if the application wants a composite look |
| Rainbow, field-dependent crawl | Subcarrier phase advance per line and field | No |
| Component YPbPr | Luma 5 to 30 MHz, chroma at about half | No |

- These effects belong to the signal domain, before the tube model. Offer them as an optional separate module, not part of the core CRT.

## Geometry

### Faceplate curvature

| Shape | Examples | Radii |
|---|---|---|
| Spherical "1R" | Most pre-1990 TVs and monitors | R = 1.767 × effective diagonal (patent definition). A 26 in (66 cm) tube gives about 1.17 m, a 14 in (35.6 cm) tube about 0.63 m [S][D] |
| Flatter spherical (1.5R to 2R, "FST", "flat square") | Late-1980s/90s | 1.5 to 2 times the 1R radius; the exact FST value is not confirmed. Inner surfaces range from 1.2R to 8R in patents [S] |
| Cylindrical | Sony Trinitron, Diamondtron | Curved horizontally, flat vertically [S]. No Sony or Mitsubishi product radius was found. A design calculation in US 5,386,174 (individual inventor, not a product) for a 106°, 680 mm diagonal Trinitron-type panel gives an outer horizontal radius of 1594 to 2100 mm (inner 1413 to 1803 mm, about 1.3 to 1.75 times the 1R value of 1202 mm) and an outer vertical radius of 40,000 mm [S][D]. A Sony web page reportedly gives about 2,000 mm for conventional Trinitrons (search summary only, not verified) |
| Flat outer face, curved inner face | FD Trinitron, Diamondtron NF, PureFlat, DynaFlat, Flatron, Real Flat | Outer surface flat, inner curved with a large radius; the glass is thicker at the edges; the image looks slightly concave through refraction [S][K]. Inner radius examples: about 10,000 mm on a Sony 28 in flat-face tube (US Re. 39,739); 2,300 mm horizontal on a Mitsubishi 41 cm and 2,500 mm on a 50 cm flat tube, the latter with an outer radius of 50,000 mm (US 6,133,686) [S] |

- Corner sagitta s = R − √(R² − (D/2)²): [D]

| Tube | R / diagonal | Corner sagitta / diagonal | For a 19 in viewable tube |
|---|---|---|---|
| 1R | 1.767 | 7.2 % | R = 853 mm, sagitta 35 mm |
| 1.5R | 2.65 | 4.8 % | R = 1280 mm, sagitta 23 mm |
| 2R | 3.53 | 3.6 % | R = 1706 mm, sagitta 17 mm |
| 8R | 14.1 | 0.9 % | R = 6820 mm, sagitta 4 mm |

- Faceplate radius grew from 30 in to 68 in over the history of the CRT before going flat. Many late panels are aspherical (at least 10 % more curvature along the short axis in one design). [S]
- The visible curvature is that of the phosphor surface (inner face) seen through the glass, plus refraction. A physically based path ray-casts from the eye through the outer surface (n about 1.53) to the inner surface. [D]
- crt-geom defaults R = 2.0, d = 1.5 in normalised coordinates, roughly a 1R tube viewed from 1.5 screen widths. [S][D]
- Typical monitor viewing distance 50 to 65 cm is 1.0 to 1.3 diagonals of a 19 in tube. [S][D]

### Raster distortions and corrections

| Distortion | Cause | Correction | Typical residual |
|---|---|---|---|
| Pincushion (E-W, N-S) | Deflection geometry on a flatter-than-sphere screen | Diode modulator or parabola in size (E-W); yoke field design (N-S); one datasheet allows up to 10 % E-W correction | under 0.5 to 1 % on monitors, 1 to 3 % on TVs (2 to 5 mm) |
| Barrel | Over-correction | | same |
| Trapezoid (keystone) | Asymmetric yoke or correction | Geometry controls | under 1 % |
| Parallelogram | Yoke rotation between H and V coils | Tilt/rotation coil | under 1 % |
| Pin balance (bow) | Asymmetry | | under 1 % |
| Rotation (tilt) | Earth's field (about 0.5 gauss) | Rotation coil (0.95° per 75 mA in one datasheet) | up to 1 to 2° uncorrected |
| S-linearity, horizontal and vertical | Beam position ∝ tan θ | S-correction capacitors, linearity coil | 0.5 to 2 % of picture height; 2 to 5 % horizontal on TVs; BVM-D20 under 0.5 % in the central circle, about 1 % outside |
| Corner and "gull wing" terms | Flat or slim wide-angle tubes | | worse on flat tubes; "magic carpet" waviness |
| Raster size variation with brightness | EHT sag | Regulation | see blooming |

- Position mapping: for a flat screen at distance L from the deflection centre, x = L·tan θ. S-correction makes screen speed constant. For a sphere centred on the deflection centre, position = R·θ. [K]
- Raster centring tolerance ±5.5 mm on a 21 in tube; the undeflected spot must land within a 2 mm radius circle. [S]
- PC monitors expose pincushion, trapezoid, parallelogram, pin balance, rotation and corner controls with a few mm of range. [K]

### Visible area, overscan, corners

- Overscan: Sony BVM-D 5 % normal scan and 3 % underscan inside the mask; PVM-20S1WU 7 %; consumer TVs typically 5 to 10 % per axis (safe-action 90 %, safe-title 80 %). PC monitors are usually underscanned with a user-adjustable black border. [S][E]
- The visible opening is a rounded rectangle defined by the tube's phosphor area and the cabinet bezel. Corner radius is about 15 to 40 mm (3 to 8 % of screen height) on older tubes and near zero on flat ones. [E]

### Convergence, purity and landing

| Device | Misconvergence |
|---|---|
| Sony BVM-D20 | under 0.4 mm inside a central circle of diameter equal to picture height, under 0.7 mm elsewhere [S] |
| Sony BVM-D32 | under 0.5 mm / 0.8 mm [S] |
| Sony BVM-8045QD | 0.43 mm centre, 0.53 mm periphery [S] |
| Sony PVM-20S1WU | 0.6 mm centre [S] |
| 21 in monitor tube datasheet | 0.30 mm centre and 0.50 mm corners, or 0.2 mm everywhere with dynamic convergence coils [S] |
| PC monitors in general | 0.2 to 0.3 mm centre, 0.3 to 0.4 mm corners [E] |
| Consumer TVs | 0.5 to 1.5 mm, up to 2 mm in corners, 2 to 3 mm in cheap sets [E] |

- Misconvergence is "generally much larger than the dot pitch". [S]
- In in-line systems red and blue errors are roughly mirror images about green, mostly horizontal, growing with distance from the centre. [E]
- Model: static centre offset plus a quadratic polynomial in x and y, per channel. [E]
- Purity: the beam must land on its own phosphor. Errors show as colour blotches, mostly near corners, from stray magnetisation (cured by degaussing), the Earth's field (about 0.3 to 0.6 gauss, tens of µm of shift) or doming. [S][E]

## Temporal behaviour

### Raster timing

| System | Total / active lines | Line rate | Field rate | Line period | Pixel clock |
|---|---|---|---|---|---|
| NTSC 480i | 525 / about 480 | 15.734 kHz | 59.94 Hz | 63.56 µs (active about 52.6 µs, H-blank about 10.9 µs) | n/a |
| 240p consoles | 262 or 263 / 224 to 240 | about 15.7 kHz | 59.2 to 60.1 Hz | about 63.6 µs | 5.37 to 8 MHz |
| PAL 576i | 625 / 576 | 15.625 kHz | 50 Hz | 64 µs (active about 52 µs) | n/a |
| VGA 640×480 | 525 / 480 | 31.469 kHz | 59.94 Hz | 31.78 µs | 25.175 MHz |
| VGA 320×200 (mode 13h) | 449 / 400 doubled | 31.469 kHz | 70.09 Hz | 31.78 µs | 12.59 MHz effective |
| SVGA 800×600 @ 85 | 631 / 600 | 53.67 kHz | 85 Hz | 18.6 µs | 56.25 MHz |
| XGA 1024×768 @ 85 | 808 / 768 | 68.68 kHz | 85 Hz | 14.6 µs | 94.5 MHz |
| UXGA 1600×1200 @ 85 | 1250 / 1200 | 106.25 kHz | 85 Hz | 9.41 µs | 229.5 MHz |

- NTSC and PAL line rates are confirmed [S]. The VGA rows are standard timings from memory. Console timings vary per system.
- The beam takes about 92 % of the field period top to bottom, the rest is vertical retrace (about 20 lines of 262.5 on NTSC). The bottom is drawn about 15 ms after the top at 60 Hz. [S][K]

### Scan in time

- One frame is not instantaneous. At any instant only a thin band is lit. Visible brightness is the time integral. [D]
- The CRT is an impulse display with a duty cycle of a few percent. [S]
- **Peak versus average luminance:** L_peak ≈ L_avg · T_frame / τ_eff. With 100 cd/m² average at 60 Hz: about 33,000 cd/m² for green/blue with τ_eff of about 50 µs (fast term only), about 3,300 to 4,000 cd/m² for red with τ_eff of 0.4 to 0.5 ms. Real values are lower because the sulfide tail carries part of the energy. The order of magnitude is 10³ to 10⁴ cd/m². [D]
- Motion: persistence of about 1 ms means tracked motion blur is about 1 pixel per 1000 pixels/s. A sample-and-hold display at 60 Hz blurs by 16.7 ms times the speed. [D]
- If the frame rate is below the refresh rate, tracked objects show discrete multiple images, not blur (30 fps on 60 Hz shows a double edge). [E]
- Per-colour decay mismatch gives colour fringing on motion: red lingers longest to 10 %, green and blue leave the longest faint tail. [S][D]
- Flicker visibility rises with luminance and field of view (Ferry-Porter law). 50 Hz flickers visibly, 60 Hz on large bright fields, at least 72 to 85 Hz is considered flicker-free for monitors (VESA/TCO). Some PAL sets double to 100 Hz. CRT refresh is practically limited to about 160 Hz. [S][K]
- Interlace: horizontal detail one line high lit in only one field flickers at 30/25 Hz (twitter); fields woven in a digital capture show combing on moving edges. [E]
- Talbot-Plateau law: above fusion frequency, perceived brightness is the time-averaged luminance, so a frame-averaged model is valid for still content on a 60 Hz host. [K]

### Implications for output refresh rate

| Host rate | What can be represented |
|---|---|
| 60 Hz | Only the time-integrated frame. Persistence can only appear as a frame-to-frame trail: previous frames weighted by the per-frame energy ratios E_k/E0 of "Decay and persistence" (about 5 %, 3 %, 2 % for green and blue at k = 1, 2, 3; red below 1e-6) [D] |
| 120 / 240 / 480 Hz | N sub-frames per CRT frame. In sub-frame k only the band of lines scanned during [k/N, (k+1)/N) is lit, plus decaying contributions from earlier bands (Blur Busters approach). Blur reduction for 60 fps content is about 50 %, 75 % and 87.5 % |

- cgwg's estimate for convincing temporal emulation: 4 sub-frames at 240 Hz, 8 at 480 Hz, 16 at 960 Hz. [S]
- Energy in a sub-frame window [a, b] for a point last excited at t0: L0·[F_c(b − t0) − F_c(a − t0)] with F_c the integral of the decay f_c (closed form for exponentials). Slow components carry across sub-frames. [D]
- Brightness must be conserved. A pure impulse needs N times the peak luminance, so HDR headroom is required. The Blur Busters shader spreads bright pixels across sub-frames (GAIN_VS_BLUR 0.7 at 240 Hz, 0.5 at 120 Hz). [S]
- All temporal accumulation must be done in linear light, and the output display's gamma must match the assumed one, or horizontal bands appear. [S]
- The source's own cadence (for example 60.1 Hz from a console) rarely equals the output refresh, so beat-frequency judder or tearing must be handled by a pacing design. [E]
- Persistence as a per-channel low-pass on an HDR accumulation buffer: S_c(t+dt) = S_c(t)·exp(−dt/τ_c) + E_c(dt), plus a power-law term. [D]
- **Implemented (2026-11-27, `scan_mode`, `crt_model::scan`; REFERENCE.md "Motion clarity"):** the sub-frame rows above are the rolling scan of Phoxium: at refresh k of N the band of lines the beam crosses is lit at the gain 1 / duty (N for the rolling scan, `scan_peak_boost` 1) and the phosphor stage decays it in real time, so the afterglow of a band carries into the next refreshes; `scan_mode = strobe` is the black frame insertion row. The peak limit is the panel's: a line lit N times brighter clips above 1 / N of white on an SDR output (`scan_peak_boost` chooses how much of the loss to give back); with persistence on the mean light over the sweep of a still picture is the frame's light (tested at 300 Hz and five refreshes, within 0.03 % after the history settled), the red (tau 0.44 ms, no tail) follows the band (tested: at the first refresh of a sweep the top fifth carries more than five times the red of the bottom fifth), and the green and blue keep part of each band's energy into the following refreshes through their slow terms (by the fit; not measured separately here). [D]

## Artefact catalogue (cause and order of magnitude)

| Artefact | Physical cause | Order of magnitude |
|---|---|---|
| Visible scanlines (240p) | Spot smaller than line pitch | 57 % modulation at σ/P = 0.25, under 2 % at 0.5 |
| Scanline thickness varying with brightness | Spot grows with beam current | Width ×1.5 to 2.5 from black to white, current exponent 0.3 to 0.5 |
| Phosphor mask texture | Discrete elements in black matrix | Pitch 0.22 to 0.9 mm; 15 to 27 % of area per primary |
| Brightness loss through mask | Electrons stopped by the mask | 75 to 85 % of the beam |
| Moiré | Beat of pixels or lines with the mask | Period p1·p2/|p1 − p2|; visible above about 1.4 % |
| Horizontal softness | Amplifier rise time plus spot width | 0.1 to 0.6 source pixels (amplifier), 0.4 to 1.5 mm FWHM (spot) |
| Dim vertical one-pixel lines | Bandwidth limit before the gun nonlinearity | 5 % to over 50 % luminance loss |
| Horizontal ringing, outline | Peaking, SVM, cable reflections | 5 to 20 % overshoot |
| Glow around bright areas (bloom) | Spot tails, defocus at high current | Radius 1 to 3 spot widths |
| Halation | Total internal reflection in the faceplate | Ring radius 1.75 × thickness, about 17 to 25 mm, level about 0.5 to 5 % |
| Veiling glare | Scattered light and electrons inside the tube | ANSI contrast 100 to 150:1 |
| Raster breathing | EHT sag with beam current | Under 1 % (broadcast), 1 to 5 % (consumer) |
| ABL dimming | Average-current limiter | Full white 30 to 60 % of small-window peak on TVs |
| Misconvergence fringes | Beams landing apart | 0.2 to 0.4 mm centre, 0.4 to 0.8 mm edges, up to 2 mm on TVs |
| Corner defocus and haze | Deflection defocus and astigmatism | Spot ×1.3 to 2 horizontally |
| Vignetting | Oblique landing, mask grading, glass thickness | Corners 70 to 90 % of centre |
| Colour impurity patches | Landing errors, magnetisation, doming | Mask shift 10 to 100 µm |
| Damper-wire shadows | Wires across the aperture grille | 1 to 3 faint lines, 10 to 30 µm wires, at about 1/3 and 2/3 height |
| Phosphor trails | Power-law sulfide afterglow (green, blue) | About 5 % of a frame's green and blue energy in the next frame at 60 Hz, 3 % and 2 % in the two after (one measured monitor; Ni-killed phosphors lower); red negligible |
| Flicker | Impulse emission | Visible below about 75 Hz on bright fields |
| Gamma and black-level shifts | Brightness control offset in the gun law | Effective gamma 2.4 to 2.6; darker mid-tones than sRGB |
| White-point tint | Gun gain ratios | 6500 K to 9300 K and beyond |
| Highlight colour shift | Sulfide saturation | A few percent to a few hundred K warmer |
| Glass reflections | Front-surface specular | 4.3 % bare, 0.5 to 2 % coated |
| Raised blacks in a lit room | Diffuse screen reflection | About T² × 15 to 40 % of ambient |
| Geometry errors, curvature | Deflection, faceplate shape | 0.5 to 3 % of picture height; corner sagitta 3.6 to 7.2 % of the diagonal |
| Raster tilt | Ramp vertical deflection, Earth's field | About one line pitch over the width; up to about 1° |
| Line jitter and swim | Deflection noise, mains interference | Under 0.1 mm on good monitors |
| Burn-in and ageing | Phosphor and cathode wear | 15 % loss in 20,000 h in one datasheet |
| Interlace twitter | 480i/576i | 30/25 Hz |
| Dot crawl, rainbow | Composite only | Not present on RGB |

## Existing emulation and shader work

| Work | What it models | Known limitations |
|---|---|---|
| crt-geom (cgwg, 2009 onward) | Per-channel scanline beam whose shape depends on intensity, Lanczos horizontal filter, sphere-ray curvature with radius and viewing distance, tilt, rounded corners, two-colour dot mask, CRT gamma 2.4 against monitor 2.2, interlace detection | Mask is an LCD-pixel pattern, not a phosphor geometry. No halation, convergence or temporal model. Lanczos rings in linear space |
| crt-geom-deluxe (cgwg) | Adds mask choices, halation, raster bloom and a power-law phosphor decay with frame feedback | Tuned by eye |
| crt-lottes (Timothy Lottes, 2014) | Single pass: Gaussian pixel and scanline kernels, wide bloom, four mask styles, barrel warp | Fixed kernels, masks in output pixels, no beam dynamics |
| crt-royale (TroggleMonkey, 2014) | Generalised-Gaussian beam with σ from 0.02 to 0.3 scanlines and shape exponent from 2 to 4 as a function of intensity, per-channel convergence, mask textures (aperture grille, slot, EDP shadow) Lanczos-resampled to any triad size, halation, diffusion, bloom that restores mask brightness loss, sphere and cylinder geometry, interlacing | Heavy. Bloom is a brightness trick, not physics. Triad size is in output pixels. Needs 4K to 8K for shadow mask. No temporal model |
| crt-guest-advanced (guest.r, Dr. Venom, 2018 onward) | Brightness-dependent beam width and shape, many mask layouts including subpixel-aware, halation, bloom, deconvergence, raster bloom, per-colour afterglow, colour profiles (P22/SMPTE-C/EBU), white point, NTSC variants | Empirical, huge parameter space, units are output pixels and unitless strengths |
| crt-beans (2023 onward) | Analog low-pass with bandwidth in MHz, spot size relative to scanline height, wide-kernel glow | Author says scanline dynamics are "not as accurate" |
| GTU (aliaspider) | Bandwidth-limited signal, integrated Gaussian scanlines | No mask |
| crt-hyllian, easymode, aperture, zfast-crt, crt-pi, crt-caligari | Lightweight scanline plus mask, or additive overlapping spots | Not physical |
| MAME HLSL / BGFX | Mask texture overlay, scanlines, defocus, linear and radial convergence, per-channel exponential persistence, multi-level bloom, curvature, vignette, reflection, NTSC YIQ | Everything is an independent artist control. Persistence is a simple exponential |
| Sony Megatron (MajorPainTheCactus, 2021 onward) | HDR peak compensates mask loss instead of bloom; masks laid out for the panel's own subpixel order (RGB, BGR, WOLED, QD-OLED); presets named after PVM/BVM models | Needs an HDR panel of 600 to 1000+ cd/m². Little beyond mask and scanlines |
| HSM Mega Bezel, koko-aio | Bezel, glass reflections, ambient light layered on other cores | Cosmetic |
| Grade (Dogway) | Phosphor gamuts, white-point temperature, BT.1886 Annex 1 CRT gamma with black level, vignette | Colour only |
| gamutthingy (ChthonVII) | Measured phosphor sets and white points with citations, gamut mapping, TV colour-correction circuits | Colour only |
| Blur Busters CRT Beam Simulator (Rejhon and Lottes, 2024, MIT; ShaderBeam app) | Temporal only: rolling scan with phosphor fade over sub-frames, per-channel persistence, linear-light accumulation with gamma 2.4 | Needs 120 Hz or more, SDR with ABL off, fast pixels. Banding if display gamma mismatches. Not for strobed, FALD or mini-LED panels. Keeps three frames of history |
| Mattias Gustavsson crtemu, newpixie-crt, cool-retro-term | Artistic curvature, vignette, ghosting, flicker, bleed | Not physical |
| Blargg NTSC, ntsc-adaptive, NTSC-CRT, ntsc-rs | Composite encode and decode, artifact colours | Signal only; comb filters poorly covered |
| ShaderGlass | Windows overlay that applies RetroArch shaders to a captured window | Closest existing product to the Phoxium use case; inherits the shaders' limits |
| Analog TV Simulator, CRT-Standalone (ReShade) | Marketing claims of datasheet-parameterised stages; aggregation of other shaders | Not validated publicly |

Common limitations of the field, as stated by cgwg in 2019: [S]

- Almost no measured data for historical displays.
- Video amplifier effects are not simulated accurately.
- Spot size as a function of intensity lacks empirical data.
- Spot filters are symmetric although the real response is not.
- No shader models brightness as a function of the lit fraction of the screen (ABL).
- Masks ignore the output panel's own subpixel structure.
- Colour corrections based on "looks right" are untrustworthy. Primaries and white points should come from a colorimeter or manufacturer data.

**Physically based and scientific literature**

| Reference | Content |
|---|---|
| Garcia-Pérez and Peli, Spatial Vision 14 (2001) | Horizontal against vertical gratings, persistence, bandwidth, DC restoration, EHT regulation, uniformity, size dependence of calibration |
| Elze, J. Neurosci. Methods (2010) | Measured CRT and LCD temporal signals, P22 decay, raster timing |
| Bach, Meigen and Strasburger, Spatial Vision 10 (1997) | Limits of raster CRTs in space, time and intensity |
| Hu and Klein, SPIE 2179 (1994); Klein, Hu and Carney, Vision Research (1996); Mulligan and Stone, JOSA A (1989); Pelli, Spatial Vision 10 (1997) | Adjacent-pixel nonlinearity |
| Badano and Flynn, SPIE and Medical Physics (1997 to 1999) | Veiling glare measurement and Monte Carlo light transport in faceplates |
| Berns, Motta and Gorzynski, Color Res. Appl. 18 (1993) | Gain-offset-gamma CRT colorimetry |
| Brainard, Pelli and Robson (2002) | Display characterisation: phosphor constancy and channel independence |
| Golz and MacLeod (2003) | Chromaticities of 15 monitors from eight manufacturers |
| Vingrys and King-Smith (1986); Zele and Vingrys (2005) | Colour-dependent phosphor decay and duty-cycle effects |
| ITU-R BT.1886 (2011) | Reference EOTF derived from CRT measurements |
| RCA patent US 4,631,691 (1986) | Digital simulation of a display including an electron-spot convolution step |
| Bell System Technical Journal 40 (1961); SMPTE Journal 69 (1960) | Spot-size definitions and measurement |
| Keller (1991); Sherr (1993); Morrell et al. (1974); Compton (2003); Moss (1968) | Engineering books on guns, masks, phosphors and MTF; not consulted in this pass |
| Goldwasser, "TV and Monitor CRT (Picture Tube) Information" (repairfaq.org) | Practical FAQ with geometry formulas and measured pitches |
| NIDL monitor evaluations (for example Sony GDM-F500) | Independent luminance, halation, line width and convergence measurements; could not be fetched |

No peer-reviewed or open real-time renderer was found that combines a measured beam profile, a geometric mask model, measured phosphor spectra and decay, and faceplate light transport.
That is the gap Phoxium could fill.

## Suggested physical pipeline order (informative)

1. Decode the captured frame (sRGB or the source's assumed curve) to code values. Optional signal-domain stage: composite filter, amplifier bandwidth with rise/fall asymmetry in the voltage domain, peaking.
2. Apply the gun law per channel (GOG or BT.1886 with brightness, contrast and cutoff), then ABL (frame-APL-based gain, low-passed in time), giving beam current per channel along the line.
3. Beam: per scanline, Gaussian (plus tail) in y with σ(I, x, y), horizontal kernel σ_x(I, x, y), per-channel convergence offsets and geometry (curvature, pincushion residuals, blooming scale).
4. Mask: integrate the beam over each emitter (prefiltered to the host-pixel footprint) and multiply by emitter shape and fill factor. Apply phosphor saturation.
5. Phosphor: temporal decay (afterglow term on a 60 Hz host, rolling sub-frames at 120 Hz or more).
6. Optics: phosphor-layer diffusion, halation ring, veiling glare (proportional to APL), vignetting and uniformity, tint.
7. Convert from phosphor primaries and white to the host display primaries in linear light, then encode for the host (SDR, or HDR which gives mask-loss headroom). Optional reflections and bezel.

## Starting presets for shader parameters

All values are defaults for sliders. Spot sizes and halation are the least certain entries.

| Preset | Mask | Pitch (mm) | Visible w × h (cm) | Spot FWHM mid → peak (mm) | Halation t (mm) / weight | Primaries | White | γ |
|---|---|---|---|---|---|---|---|---|
| Consumer 26 in viewable TV (slot) | slot, vertical period about 1.6 p | 0.80 | 52.8 × 39.6 | about 1.0 → 1.7 | 13 / 3 % | SMPTE-C-ish P22 | 9300 K (JP) or 7500 to 9300 K (US) | 2.4 to 2.5 |
| Sony KV Trinitron TV | AG | 0.75 to 0.8 | 51 × 38 | about 0.9 → 1.6 | 13 / 2 % | Trinitron (0.625/0.34, 0.28/0.595, 0.155/0.07) | 9300 K | 2.4 to 2.5 |
| PVM-14L2 | AG | 0.25 | 26.4 × 19.8 | 0.4 → 0.7 | 10 / 1.5 % | SMPTE-C or EBU | D65 or D93 | 2.4 |
| PVM-20M4U | AG | 0.31 | 38.6 × 29 | 0.45 → 0.8 | 12 / 1.5 % | SMPTE-C or EBU | D65 or D93 | 2.4 |
| BVM-20F1U | AG | 0.30 | 38.6 × 29 | 0.4 → 0.7 | 12 / 1 % | SMPTE-C / EBU / 709 | D65 | 2.4 (BT.1886 origin) |
| 15 in VGA monitor | delta dot | 0.28 diagonal (0.242 horizontal) | 28 × 21 | 0.3 → 0.45 | 10 / 2 % | P22 (about SMPTE-C) | 9300 K default | 2.35 to 2.5 |
| 17 to 21 in Trinitron / Diamondtron monitor | AG (FD: 0.22 uniform; FW900 0.23 to 0.27) | 0.22 to 0.26 | 32 × 24 to 40 × 30 | 0.35 → 0.5 | 12 to 20 / 1 % | Trinitron or sRGB-ish | 9300 or 6500 K | 2.4 to 2.5 |
| NEC CromaClear 17 to 19 in | elliptical slot | 0.25 | 32 × 24 | 0.35 → 0.5 | 12 / 1.5 % | P22 | 9300 K | 2.4 |
| 19 in arcade (in-line slot) | slot | 0.7 to 0.8 | 36 × 27 | about 0.9 → 1.6 | 12 / 3 % | P22 | about 9300 K | 2.4 to 2.5 |

Other defaults:

```
Gun gamma           2.4 (range 2.2 to 2.6); per-channel cutoff offset +/- 0 to 5 % of drive
Beam shape          generalised Gaussian p = 2.0 at low drive, 2.3 to 2.8 at high drive; tail 2 to 10 % weight
Spot growth         d^2 = d0^2 + k * I^(2n), n 0.2 to 0.5 (T-63); width x1.5 to 2.5 black to white
Overscan            3 to 8 % each axis (TV); underscan available on PVM/BVM
Convergence error   centre 0 to 0.15 mm, corners 0.2 to 0.5 mm (PC/PVM) or 0.5 to 1.5 mm (TV)
Vignette            corners 0.7 to 0.9 of centre
Halation            ring radius 1.74 t, total weight 0.5 to 5 %
ABL                 threshold about 0.5 to 2 mA average (1.4 mA sourced TV example); onset about 0.1 s, settled about 0.5 s (T-63); up to 30 to 50 % contrast loss at full-field white (estimate)
Raster blooming     +1 to 5 % size at full white (unregulated), under 1 % (regulated)
Glass transmission  clear 0.85, grey 0.72, dark 0.45 to 0.55
Faceplate reflect   bare 4.3 %, AR 0.5 to 2 %, etched under 1 % specular with haze
Corner radius       15 to 40 mm
```

## Resolved conflicts

1. **Delta-mask geometry (Sonnet agent was wrong).**
   - Sonnet stated that the same-colour horizontal period in a hexagonal lattice equals P and the row spacing is 0.866 P.
   - Goldwasser (sourced by Fable) and the Opus agent give: same-colour horizontal repeat in one row = P·√3, "horizontal dot pitch" = 0.866 P, row spacing = P/2.
   - I checked this geometrically. For a triangular same-colour lattice with nearest-neighbour distance P oriented with NN vectors at ±30° from horizontal (the delta-triad layout), one NN vector is (0.866 P, 0.5 P). The in-row repeat is therefore 1.732 P and the row spacing is P/2.
   - Sonnet's reading holds only for the other lattice orientation, which is not the delta triad layout.
   - The vendor "horizontal dot pitch" is the horizontal projection of the diagonal nearest-neighbour distance, so triad density across the screen is 1/(0.866 P).
2. **Beam spot size (Opus values not adopted).**
   - Opus gives 5 %-width spot sizes of 0.15 to 0.3 mm for monitors and 0.3 to 0.5 mm for PVM/BVM, which convert to FWHM of about 0.07 to 0.14 mm and 0.15 to 0.24 mm.
   - Those are smaller than the mask pitch itself. A sourced datasheet gives 0.4 to 0.6 mm FWHM for a 21 in monitor-class tube, and a 40 cm colour tube gives 0.36 mm FWHM (0.65 mm at 10 %).
   - Sonnet's PVM values (0.5 to 1.2 mm) are higher than the Fable estimate (0.4 to 0.7 mm).
   - Adopted: monitors 0.3 to 0.5 mm, PVM/BVM 0.4 to 0.7 mm at mid drive, consumer TVs 0.7 to 1.5 mm. These bracket the sourced data points and the scanline-visibility argument (famous deep PVM scanlines imply σ/P of about 0.2 to 0.25).
3. **Phosphor decay (reconciled with a two-component model).**
   - Sources split into "fast" (AST/APACE: 35 µs green, 80 µs blue, 850 µs red to 10 %) and "slow" (Sherr 1993: 6, 4.8, 1.5 ms).
   - The Sony patent residuals (1 % at 7.5 and 7.0 ms for green and blue) cannot be matched by either number alone. They fit a fast initial drop plus a power-law tail of about 0.5 to 1 % at 8 to 16 ms.
   - The model in "Decay and persistence" is fitted to the patent residuals and gives 1 % at 7.3 ms for green.
   - The Opus agent's own default table (green, t0 = 10 µs, n = 1.2) gives about 0.014 % at 16.7 ms, not the "0.5 to 2 %" it claims. The Fable-style tail parameters were adopted instead.
   - The Sonnet default (slow green term of 800 µs with weight 0.3) is inconsistent with the patent residuals and was dropped.
   - **Revised by T-63.** The fit above was itself wrong: the patent residuals are relative to the afterglow peak after an excitation of unstated length, not to the impulse peak, and n = 0.72 gives infinite energy. Integrated over 60 Hz frames it put 29.7 % of a frame's green energy into the next frame. It is replaced by Kuhn's (2002) measured P22 impulse response (E1/E0 about 5 % green and blue at 60 Hz), which also reproduces the patent residuals for an excitation of somewhat under 1 ms.
4. **Aperture-grille transmission.**
   - Wikipedia's "Trinitron cuts off about 25 %" is flagged citation-needed. All three agents read it as 25 % transmitted, because a three-colour mask cannot pass more than one third per beam.
   - Adopted: 20 to 27 % transmitted (geometric estimate, no primary measurement found).
5. **Brightness headroom.**
   - Opus: 1.7 to 3.3 times (emissive fill 0.3 to 0.6). Fable: 400 to 700 cd/m² panel for a 100 cd/m² CRT white.
   - These measure different things (all colours together versus one primary at a time with visible scanlines). Both are kept, in "Beam transmission and brightness budget". Treat 3 to 7 times as the planning range.
6. **White points.**
   - The Sonnet agent conflated 9300 K + 27 MPCD with D93. Fable's sourced values keep them apart: 9300 K + 27 MPCD is (0.281, 0.311), 9300 K + 8 MPCD (D93) is (0.2831, 0.2971).
7. **Curvature radii.**
   - Sonnet estimated 0.9 to 1.6 m for 13 to 19 in spherical tubes. The sourced patent definition (1R = 1.767 × diagonal) gives about 0.6 to 0.85 m. The patent-based values were adopted.
8. **PVM-20M4U triad count.**
   - crt-royale's README quotes 1451 triads for 0.31 mm pitch. That implies a 450 mm width, larger than the 386 mm width of a 19 in viewable 4:3 area. Adopted 1245 triads (386 / 0.31).
9. **Doming magnitude.**
   - 100 µm (Goldwasser, sourced) versus 10 to 30 µm (estimate). Kept as a range of 10 to 100 µm and exposed as a tunable.
10. **Damper wires.**
    - One wire at 2/3 down (15 in and below), two at 1/3 and 2/3 (larger) are sourced. Goldwasser mentions three above 21 in and Sony manuals vary. The "1/3 and 2/3" rule is not universal.
11. **4K width.**
    - Fable's "3840 output pixels across" counts the full 16:9 panel width. A 4:3 image on a 2160-line panel is only 2880 px wide, which is what the host-resolution budget above uses.

## Unresolved uncertainties and data gaps

These are the places where no agent found reliable data.
They are the main inputs for QUESTIONS.md.

- **Spot size versus current for colour tubes.** One complete table exists, for a monochrome 21 in medical tube. T-63 added colour-tube points from patents (a 51 cm monitor tube at three currents, a 76 cm TV at two, a typical TV gun at 4 mA), mostly without a stated measurement level. Still no PVM/BVM data and no full curve at a defined level. Apparent exponents span 0.22 to 0.42.
- **Phosphor decay.** One measured P22 impulse response (Kuhn 2002, one monitor, records up to 2 ms) is now the default. The tail beyond 2 ms (exponent n), its variation between phosphor batches (Ni killer: more than tenfold on the 8 ms residual) and the patent's excitation length remain unknown, so E1/E0 is known to a factor of about 2, not better (estimate).
- **Consumer TV pitch, faceplate thickness, slot bridge geometry, lit-area fractions, dot diameters.** T-63 sourced RCA 1983 pitches (0.66 to 0.84 mm, 36 to 68 cm) and patent faceplate thicknesses (9 to 15 mm centre). Slot bridge geometry, lit-area fractions, dot diameters and non-RCA consumer pitches (Philips, Toshiba, Samsung, Sony KV) remain estimates.
- **Aperture-grille and slot-mask transmission.** Geometric estimates only.
- **Curvature of the cylindrical Trinitron** (horizontal radius). Still no Sony product value; only a patent design calculation (1594 to 2100 mm outer for a 680 mm diagonal) and an unverified "about 2,000 mm" from a Sony page. Flat-face inner radii are now sourced for one Sony 28 in tube (about 10,000 mm) and two Mitsubishi tubes (2,300 and 2,500 mm), not for a named FD Trinitron model.
- **Halation amplitude.** Geometry is sourced, amplitude ranges from 0.5 to 5 %. T-63 found total veiling glare (about 4 % for a colour monitor, 0.4 % for a medical one) but no ring amplitude on its own.
- **ABL threshold, time constants, consumer EHT sag, consumer convergence.** T-63 sourced one TV threshold (1.4 mA) and one onset and settling time (0.1 s, 0.5 s) from RCA patents. Contrast loss at full white, monitor ABL, EHT sag and consumer convergence remain from memory.
- **Vendor pitch values** for Hitachi EDP, Samsung DynaFlat, Panasonic and Toshiba monitors, and the per-vendor mapping to mask type (except Trinitron and Diamondtron = grille; CromaClear and the cited LG Flatron models = slot).
- **Gamma value** (2.2 to 2.8 depending on black-level convention). BBC 2.35 and Poynton 2.5 not re-verified in a primary source.
- **Video bandwidth** of consumer RGB inputs and PVM/BVM: estimates.
- **Whether a modern RGB capture should assume CRT gamma, sRGB or something else** for a given game. This is a policy question, not a physics one.
- **No open physically based real-time CRT renderer exists** to compare against or to take validated parameters from.

## Sources (condensed)

Fetched or read directly by at least one agent:

- Goldwasser, TV and Monitor CRT (Picture Tube) Information: https://www.repairfaq.org/sam/crtfaq.htm
- Wikipedia: Cathode-ray tube, Trinitron, Shadow mask, Aperture grille, Cromaclear, Dot pitch, Phosphor, Gamma correction, ITU-R BT.1886, RGB color spaces
- DisplayMate, dot pitch: https://displaymate.com/dotpitch.html
- CRT Database (model pages and articles): https://crtdatabase.com/
- Sony manuals and service documents: BVM-D series, BVM-14F5, BVM-1911, GDM-FW900, GDM-F520, 2002 display line-up, BVM-D24E1WU
- Tube datasheets: M51EDF40WB11/06, M51EDF300WB70L (tube-data.com); NEC-Mitsubishi Diamond Pro 2060u datasheet
- García-Pérez and Peli (2001): https://pelilab.partners.org/papers/monitor/artifacts_cathode.pdf
- Elze (2010): https://sites.socsci.uci.edu/~wpsych/205b/Wk6Files/Elze2010-Monitors.pdf
- Bach, Meigen and Strasburger (1997): https://michaelbach.de/misc/crtlimits/
- Palme CRT phosphor tables and AST/APACE phosphor table (SLAC)
- Kuhn, M. G., "Optical time-domain eavesdropping risks of CRT displays", IEEE Symposium on Security and Privacy 2002 (measured P22 impulse responses, equations 8 to 10): https://www.cl.cam.ac.uk/~mgk25/ieee02-optical.pdf
- RCA, Video Specifications 1983 (colour picture tube datasheets: trio spacing, faceplate transmittance): https://worldradiohistory.com/Archive-Catalogs/RCA/RCA-Video-Specifications-1983.pdf
- Flynn and Badano, "Image quality degradation by light scattering in display devices", J. Digit. Imaging 12(2), 1999 (glare ratios): https://pmc.ncbi.nlm.nih.gov/articles/PMC3452495
- Patents added by T-63 (all read on https://patents.google.com/patent/<number>): spot size US 5,814,930, US 6,605,898, US 4,529,910; ABL US 5,313,294, US 4,330,792, EP 0 394 003; faceplate US 7,176,611, US 4,537,322, US 6,133,686, US Re. 39,739 (RE39739E1); curvature model US 5,386,174
- Patents: US 5,825,124 (afterglow; https://patents.google.com/patent/US5825124), US 5,382,871 (damper wires), US 4,651,051 (18 % transmission), US 4,310,784 (halo about 3.5 t), US 4,728,856 (tint classes), US 6,960,873, US 6,160,344 and US 6,459,196 (curvature), US 5,079,477, US 4,300,157, US 5,220,239
- Bell System Technical Journal 40-3 (spot size); SMPTE Journal 69 (1960)
- libretro slang-shaders: crt-royale README and settings, crt-geom, Grade; gamutthingy constants; crt-beans forum thread; "Ten years of CRT shaders" (cgwg); Megatron thread
- Blur Busters CRT beam simulator: https://github.com/blurbusters/crt-beam-simulator
- Sound and Vision, Toshiba 34HF84 measurements; UCF chromaticity table; Golz and MacLeod (2003)
- Phosphor types (PHY-19 research pass, see "Phosphor types"): RCA, "Phosphors for Cathode-Ray Tubes", TPM-1508A, 1961 (cites JEDEC Publication 16, 1960): http://bitsavers.trailing-edge.com/components/rca/crt/TPM-1508A_RCA_Phosphors_Oct61.pdf; R. A. Bell, "Principles of Cathode-Ray Tubes, Phosphors, and High-Speed Oscillography", HP Application Note 115, 1970: https://www.qsl.net/iv3cve/hp/an/pdf/an_115.pdf; Phosphor Technology Ltd, CRT phosphors catalogue: https://www.phosphor-technology.com/crt-phosphors/; W. Wolf and H. Deubel, Spatial Vision 10(4), 323-333, 1997: https://epub.ub.uni-muenchen.de/16122/1/P31.pdf; A. V. Gallaro and R. A. Hedler (North American Philips), US 4,405,880 (yellow data-display screen) and US 4,377,768 (white), 1983; Nichia CRT phosphor list (B), Exosens phosphor screens (B); Wikipedia "Phosphor", "IBM 5151", "Monochrome monitor" and the Jankowiak compilation (C, context only)

Not retrievable in this pass: Blur Busters forum phosphor-decay thread and blurbusters.com/crt (403), Jüstel CRT phosphor table (404), NIDL GDM-F500 report (403), Emulation General Wiki CRT shaders (403), r/crtgaming spec list.

## Research pass T-63 results

Date 2026-11-18. Published data only, no instrument. Every value below is also written, with its source and tag, in the section it belongs to.

### Persistence inconsistency (resolved)

- **What was wrong: the fit.** The old green fit (A = 1, tau = 0.05 ms, B = 0.046, t0 = 1 ms, n = 0.72) anchored the Sony patent residuals (0.95 % at 8 ms, 0.6 % at 16 ms) to the peak of an impulse response. The patent measures them against "the maximum afterglow intensity peak" after an excitation of unstated length. A measured P22 impulse response (Kuhn 2002) is only 0.012 % of its peak at 8 ms, so the old fit made the tail about 75 times too strong relative to the fast part. Its n < 1 also gives infinite total energy (Kuhn: the exponent must exceed 1). [S][D]
- **The integration window was right.** E_k = integral of I(t) over [kT, (k+1)T] is the correct per-frame trail for 60 Hz frame feedback. The T-14 figure is reproduced: E1/E0 = 29.7 % for the old fit. [D]
- **The PHY-12 range was not a sourced bound on E1/E0.** Its "0.5 to 4 %" came from residual luminances (patent 0.5 to 1 %, and a "Wolf and Deubel 4 % at 20 ms" figure on a P31 phosphor that the paper does not contain: it gives below 5 % at 10 ms, below 2.5 % at 30 ms, 1.4 % at 50 ms, corrected 2026-11-21, see "Decay and persistence"), not from per-frame energy ratios. [D]
- **Corrected constants:** Kuhn's measured impulse responses (table in "Decay and persistence"). E1/E0 at 60 Hz = 5.36 % green, 4.72 % blue, below 1e-6 red; E2/E0 = 2.97 % and 2.60 %; E3/E0 = 2.03 % and 1.78 %. The same constants reproduce the patent residuals for an excitation of somewhat under 1 ms, and match the order of magnitude of the 8.5 % stereo crosstalk measured at 122.6 Hz (fit: 6.1 % at 120 Hz). [S][D]
- **Corrected statement (proposed for PHY-12 and the T-14 / T-41 acceptance lines, not edited here):** "The green and blue power-law tails put about 5 % of a frame's energy into the next frame at 60 Hz (E1/E0 = 5.4 % green, 4.7 % blue; then about 3 % and 2 %), from one measured P22 monitor; red is negligible. The value depends on the phosphor batch (Ni-killed sulfides cut the 8 ms residual 4 to 13 times) and on the tail beyond 2 ms, which is extrapolated. The oracle test should check the Kuhn constants against these table values, not against 0.5 to 4 %."
- **Consequences for T-41:** n = 1.1 converges slowly. About 21 % (green) and 19 % (blue) of the energy emitted within 1 s arrives after the first frame, and the extrapolated tail beyond 1 s holds about 30 % of the infinite-time green total. T-41 fits the trail within the first second only (four exponentials, within about 1 % up to 1 s) and lets the fit fade out smoothly after it instead of a hard cut at 1 s; the static-image normalisation uses the total of that realised fit (steady-state gain exactly 1), which exceeds the 1 s truncated integral by 10.9 to 16.8 %, so w0 is about 3 % below the 1 s truncated value (green 60 Hz 0.763 against 0.788). [D]

### Other topics

| Topic | Result | Tag |
|---|---|---|
| Spot size versus current, colour tubes | 0.45 / 0.57 / 0.70 mm at 100 / 300 / 500 µA (Hitachi 51 cm monitor tube); about 0.9 mm at 0.5 mA and 1.9 mm at 3.0 mA (Panasonic 76 cm TV); 2.5 to 3.6 mm at the 10 % level at 4.0 mA (typical TV gun). Apparent exponents 0.22 to 0.42; model range widened to n = 0.2 to 0.5 | [S][D] |
| Halation amplitude | Not resolved. Total veiling glare only: glare ratio 25 for a colour monitor (about 4 % stray light), 240 for a medical monitor (0.4 %). Range 0.5 to 5 % kept | [S][E] |
| Consumer pitch | RCA 1983: 0.66 mm (36 cm), 0.84 mm (51 cm, three types), 0.82 mm (66 cm), 0.84 mm (68 cm); faceplate transmittance 84 to 85.5 % | [S] |
| Faceplate thickness | 9 to 11.5 mm (400 to 500 mm diagonal) and 10 to 12.4 mm (570 to 700 mm) claimed by LG.Philips; 12.0 mm on a Toshiba 26 in; 12 / 14 mm on Mitsubishi 41 / 50 cm flat tubes; 15 mm centre, 18 mm edge on a Sony 28 in flat tube | [S] |
| Trinitron cylindrical radius | Not resolved. Only a patent design calculation (outer horizontal 1594 to 2100 mm, 680 mm diagonal, not a product) and an unverified Sony "about 2,000 mm" | [S] weak |
| Flat-face inner radius | About 10,000 mm (Sony 28 in); 2,300 mm and 2,500 mm horizontal (Mitsubishi 41 and 50 cm) | [S] |
| ABL | 1.4 mA average threshold (RCA TV), lowered to 1 or 0.7 mA in a peak mode; onset about 0.1 s, steady state about 0.5 s; prior-art filter about 5 fields. Contrast loss at full white not found | [S] |

### Preset values to change (input to T-64)

1. **All presets (all P22):** replace the decay constants by the Kuhn red, green and blue constants of "Decay and persistence" [S][D]. If a tail-strength slider is exposed, a range of about 0.07 to 1 times the power-law term follows the patent's Ni-killer residual ratios (0.072 / 0.966). Applying that ratio to the power-law term alone is an assumption [E].
2. **Consumer 26 in viewable TV (slot):** pitch 0.80 → 0.82 to 0.84 mm (RCA 66 cm 0.82, 68 cm 0.84) [S]; faceplate t 13 → 12 mm (Toshiba 26 in 12.0 mm; LG.Philips claim 10 to 12.4 mm) [S]; glass transmission 0.85 (RCA clear faceplates 84 to 85.5 %) [S]; ABL threshold 1.4 mA average, onset 0.1 s, settling 0.5 s [S].
3. **Sony KV Trinitron TV:** faceplate t 13 → 12 mm (generic sourced value for the size; no Sony-specific data) [S]; ABL threshold, onset and settling as item 2 (RCA values, not Sony) [S]; horizontal radius stays an estimate (about 1.3 to 1.75 × 1R from the design calculation only) [E].
4. **19 in arcade (slot):** faceplate t 12 → 11 mm (LG.Philips 9 to 11.5 mm for 400 to 500 mm diagonals) [S]; pitch unchanged (no arcade tube data; RCA 20V TV tubes are 0.84 mm) [E].
5. **PVM-20M4U and BVM-20F1U:** faceplate t 12 → 11 mm (same claim range) [S].
6. **17 to 21 in Trinitron / Diamondtron monitor (FD / NF variant):** faceplate centre t 12 to 15 mm (Mitsubishi 41 / 50 cm flat tubes 12 / 14 mm) instead of 12 to 20 mm [S]; inner horizontal radius 2,300 to 2,500 mm for Diamondtron NF-class tubes [S].
7. **Spot growth exponent n:** monitor-class presets about 0.27 (Hitachi 51 cm), TV presets about 0.42 (Panasonic 76 cm), within the widened 0.2 to 0.5 range [S][D].
8. **No change:** halation weights (no ring amplitude found), PVM-14L2 and 15 in VGA faceplate t = 10 mm (no sourced value below 400 mm diagonal; 10 mm stays an estimate), spot FWHM defaults (sourced colour points 0.45 to 0.70 mm on monitors and about 1.4 to 2 mm FWHM at 3 to 4 mA on TVs are consistent with them).

### Still unresolved

- Halation ring amplitude on its own; only total glare is sourced.
- Cylindrical Trinitron horizontal radius for any named product.
- Contrast loss at full white under ABL, monitor and PVM ABL constants, EHT sag.
- The decay tail beyond 2 ms (Kuhn's records stop there) and per-batch tail variation; only a photodiode on a real tube settles them (SCP-2, SP-13).
- Non-RCA consumer pitches (Philips, Toshiba, Samsung, Sony KV), slot bridge geometry.
- Leads not opened (name resolution failed): Woods and Tan 2002 (Curtin, stereo ghosting per colour), Elze 2010 (UCI copy), Wolf and Deubel 1997 full text, the Sony FD Trinitron technology page, the Mitsubishi Giho 1999 Diamondtron NF paper, the 1986 Philips T8 colour tube handbook.
