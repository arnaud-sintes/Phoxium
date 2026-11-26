# Third-party notices

Upstream licence notices of third-party code built into Phoxium that the crate packages do not carry
themselves. `scripts/gen-third-party.ps1` (called by `scripts/package.ps1`) writes the THIRD-PARTY.md of the
release zip: the licence texts of every crate, copied from the crate packages, followed by this file. The licence
policy (permissive only, no GPL, LGPL or AGPL) is checked by `cargo deny check licenses` with `deny.toml` and by
`scripts/licence-policy.ps1`.

## Dear ImGui crates (`dear-imgui-sys`, `dear-imgui-rs`, `dear-imgui-ash`, `dear-imgui-build-support` 0.22.0)

The four crates, by Mingzhen Zhuang (the author named in their manifests, repository <https://github.com/Latias94/dear-imgui-rs>), declare `MIT OR Apache-2.0` and ship no licence file (checked at T-08 and again for 0.22.0; their READMEs only point to `LICENSE-MIT` and `LICENSE-APACHE` at the repository root). Phoxium uses them under the MIT terms. The notice below is the repository's `LICENSE-MIT` at the commit the 0.22.0 packages were built from (`7424350518efb54693b8acd4a417b69d9d0f015b`, from their `.cargo_vcs_info.json`).

```
MIT License

Copyright (c) 2025 Mingzhen Zhuang

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## Dear ImGui and cimgui (the C and C++ sources the crates vendor)

The settings menu and the stats overlay (T-52a) use Dear ImGui 1.93.1 WIP (docking branch) through cimgui,
vendored and compiled from source by `dear-imgui-sys` (C++ with MSVC). The vendored C and C++ sources carry the notices
below (copied from the `dear-imgui-sys` 0.22.0 package). The stack-layout shim of `dear-imgui-sys`
(`THIRD_PARTY_NOTICES.md` of that package, MIT) is compiled only with its `stack-layout` feature, which
Phoxium does not enable.

### Dear ImGui (`third-party/cimgui/imgui/LICENSE.txt`)

```
The MIT License (MIT)

Copyright (c) 2014-2026 Omar Cornut

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

### cimgui (`third-party/cimgui/LICENSE`)

```
The MIT License (MIT)

Copyright (c) 2015 Stephan Dilly

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## ProggyClean (default font data inside Dear ImGui)

`imgui_draw.cpp` of the vendored Dear ImGui embeds the data of the bitmap font ProggyClean.ttf (`[SECTION] Default
font data (ProggyClean.ttf)`), so it is part of the executable although the menu draws with Inter. The notice in
that file reads "MIT License / Copyright (c) 2004, 2005 Tristan Grimmer" (<https://github.com/bluescan/proggyfonts>).
The MIT text is the one of Dear ImGui above, with this copyright line:

```
MIT License

Copyright (c) 2004, 2005 Tristan Grimmer

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## stb libraries inside Dear ImGui (`imstb_truetype.h`, `imstb_rectpack.h`, `imstb_textedit.h`)

Dear ImGui includes slightly modified copies of stb_truetype.h 1.26, stb_rect_pack.h 1.01 and stb_textedit.h 1.14 by
Sean Barrett (<https://github.com/nothings/stb>), which are in the public domain and also offered under the MIT
licence. The notice at the end of each of the three files (identical in all three) is:

```
------------------------------------------------------------------------------
This software is available under 2 licenses -- choose whichever you prefer.
------------------------------------------------------------------------------
ALTERNATIVE A - MIT License
Copyright (c) 2017 Sean Barrett
Permission is hereby granted, free of charge, to any person obtaining a copy of
this software and associated documentation files (the "Software"), to deal in
the Software without restriction, including without limitation the rights to
use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies
of the Software, and to permit persons to whom the Software is furnished to do
so, subject to the following conditions:
The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.
THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
------------------------------------------------------------------------------
ALTERNATIVE B - Public Domain (www.unlicense.org)
This is free and unencumbered software released into the public domain.
Anyone is free to copy, modify, publish, use, compile, sell, or distribute this
software, either in source code form or as a compiled binary, for any purpose,
commercial or non-commercial, and by any means.
In jurisdictions that recognize copyright laws, the author or authors of this
software dedicate any and all copyright interest in the software to the public
domain. We make this dedication for the benefit of the public at large and to
the detriment of our heirs and successors. We intend this dedication to be an
overt act of relinquishment in perpetuity of all present and future rights to
this software under copyright law.
THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN
ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION
WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
------------------------------------------------------------------------------
```

## Inter 4.1 (font of the settings menu and the overlay)

The UI font is Inter 4.1 by The Inter Project Authors (<https://github.com/rsms/inter>, release `Inter-4.1.zip`,
files `extras/ttf/Inter-Regular.ttf` and `extras/ttf/Inter-SemiBold.ttf`). The executable embeds Modified Versions
of both (`crates/phoxium/assets/fonts/`, made by `subset.py` in the same folder): cut to Basic Latin, Latin-1
and a few punctuation and symbol code points, digits mapped to Inter's tabular forms, hinting and layout tables
removed; the copyright and licence name records are kept. Inter declares no Reserved Font Name. Licence: SIL Open
Font License 1.1, compatible with embedding in this program and its release zip (the font is not sold by itself).

```
Copyright (c) 2016 The Inter Project Authors (https://github.com/rsms/inter)

This Font Software is licensed under the SIL Open Font License, Version 1.1.
This license is copied below, and is also available with a FAQ at:
http://scripts.sil.org/OFL

-----------------------------------------------------------
SIL OPEN FONT LICENSE Version 1.1 - 26 February 2007
-----------------------------------------------------------

PREAMBLE
The goals of the Open Font License (OFL) are to stimulate worldwide
development of collaborative font projects, to support the font creation
efforts of academic and linguistic communities, and to provide a free and
open framework in which fonts may be shared and improved in partnership
with others.

The OFL allows the licensed fonts to be used, studied, modified and
redistributed freely as long as they are not sold by themselves. The
fonts, including any derivative works, can be bundled, embedded,
redistributed and/or sold with any software provided that any reserved
names are not used by derivative works. The fonts and derivatives,
however, cannot be released under any other type of license. The
requirement for fonts to remain under this license does not apply
to any document created using the fonts or their derivatives.

DEFINITIONS
"Font Software" refers to the set of files released by the Copyright
Holder(s) under this license and clearly marked as such. This may
include source files, build scripts and documentation.

"Reserved Font Name" refers to any names specified as such after the
copyright statement(s).

"Original Version" refers to the collection of Font Software components as
distributed by the Copyright Holder(s).

"Modified Version" refers to any derivative made by adding to, deleting,
or substituting -- in part or in whole -- any of the components of the
Original Version, by changing formats or by porting the Font Software to a
new environment.

"Author" refers to any designer, engineer, programmer, technical
writer or other person who contributed to the Font Software.

PERMISSION AND CONDITIONS
Permission is hereby granted, free of charge, to any person obtaining
a copy of the Font Software, to use, study, copy, merge, embed, modify,
redistribute, and sell modified and unmodified copies of the Font
Software, subject to the following conditions:

1) Neither the Font Software nor any of its individual components,
in Original or Modified Versions, may be sold by itself.

2) Original or Modified Versions of the Font Software may be bundled,
redistributed and/or sold with any software, provided that each copy
contains the above copyright notice and this license. These can be
included either as stand-alone text files, human-readable headers or
in the appropriate machine-readable metadata fields within text or
binary files as long as those fields can be easily viewed by the user.

3) No Modified Version of the Font Software may use the Reserved Font
Name(s) unless explicit written permission is granted by the corresponding
Copyright Holder. This restriction only applies to the primary font name as
presented to the users.

4) The name(s) of the Copyright Holder(s) or the Author(s) of the Font
Software shall not be used to promote, endorse or advertise any
Modified Version, except to acknowledge the contribution(s) of the
Copyright Holder(s) and the Author(s) or with their explicit written
permission.

5) The Font Software, modified or unmodified, in part or in whole,
must be distributed entirely under this license, and must not be
distributed under any other license. The requirement for fonts to
remain under this license does not apply to any document created
using the Font Software.

TERMINATION
This license becomes null and void if any of the above conditions are
not met.

DISCLAIMER
THE FONT SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO ANY WARRANTIES OF
MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT
OF COPYRIGHT, PATENT, TRADEMARK, OR OTHER RIGHT. IN NO EVENT SHALL THE
COPYRIGHT HOLDER BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY,
INCLUDING ANY GENERAL, SPECIAL, INDIRECT, INCIDENTAL, OR CONSEQUENTIAL
DAMAGES, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
FROM, OUT OF THE USE OR INABILITY TO USE THE FONT SOFTWARE OR FROM
OTHER DEALINGS IN THE FONT SOFTWARE.
```
## MAME `a7800.cpp` palette values (reference colours of the Atari 7800 target)

`presets/targets/atari_7800_maria.toml` holds, as the colours its recognition compares with, the 256 RGB values of the palette labelled 'PALETTE - 25.7 PHASE SHIFT' in `src/mame/atari/a7800.cpp` of MAME (<https://github.com/mamedev/mame>). The file's header declares `license:BSD-3-Clause` and `copyright-holders:Dan Boris, Fabio Priuli, Mike Saarna, Robert Tuccitto` (MAME as a whole is GPL, which is why only these numbers are used, from this one file, and none of its code). The values are kept with this notice. The licence text below is the standard BSD 3-Clause text with the holders the file header names; MAME's own legal directory (`docs/legal`) was not read.

```
BSD 3-Clause License

Copyright (c) Dan Boris, Fabio Priuli, Mike Saarna, Robert Tuccitto

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are met:

1. Redistributions of source code must retain the above copyright notice, this
   list of conditions and the following disclaimer.

2. Redistributions in binary form must reproduce the above copyright notice,
   this list of conditions and the following disclaimer in the documentation
   and/or other materials provided with the distribution.

3. Neither the name of the copyright holder nor the names of its
   contributors may be used to endorse or promote products derived from
   this software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE
FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
```

## NEC HuC6260 colour table (the PC Engine composite target)

`presets/targets/nec_pc_engine_composite.toml` holds the 512 entries (5-bit U, V and Y, one for each 9-bit colour code) of the silicon table of the HuC6260 video colour encoder, the chip of the NEC PC Engine, the TurboGrafx-16 and the SuperGrafx. The table is NEC's data: furrtek read it from a real console through the chip's test pins, and Kitrinx (Jamie Blanks) published it as `pbpry_int_lut` with `index_lut` in `bfbii.h` of the repository <https://github.com/Kitrinx/TG16_Palette>. **That file carries no licence header and GitHub reports no licence for the repository** (the converter `tg16color.c` in the same repository is MIT, Copyright (c) 2020 Jamie Blanks aka Kitrinx; `color_lib.h` carries a copyright line only). The 512 numbers are shipped as hardware facts (measured values of a chip, not a creative work) with this attribution, not under a licence of their own; if the author or furrtek objects, the file can be removed and the target with it.

Also taken from the same repository, as numbers only (no code is copied): the constants of the decode that Kitrinx fitted to vectorscope plots of the 240p Test Suite colour bars on a PC Engine Duo-R (V gain 0.96, chroma plane turned by -10.4 degrees, desaturation 192/255, BT.601 maxima 0.436 and 0.615). The decode in `crates/crt-model/src/target.rs` was written independently from these numbers and checked against the converter's published `palette.pal` (all 512 entries).

Nothing was taken from the MiSTer core's palette file (GPL).

The notice of the converter that the constants come from, reproduced for completeness:

```
MIT License

Copyright (c) 2020 Jamie Blanks aka Kitrinx

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## MAME `tia.cpp` palette values (reference colours of the Atari 2600 target)

`presets/targets/atari_2600_tia.toml` holds, as the colours its recognition compares with, the 128 RGB values of the NTSC palette that `src/mame/atari/tia.cpp` of MAME (<https://github.com/mamedev/mame>) builds in `tia_ntsc_video_device::init_palette`. MAME computes that palette in code from a table of 16 (I, Q) pairs labelled 'Phase Shift 26.2', a luma of step / 7, the usual YIQ-to-RGB matrix and an exponent of 0.9; the values in the file are that computation done once with those numbers and rounded as MAME rounds them (no code is copied, and a unit test recomputes them from the table). The file's header declares `license:BSD-3-Clause` and `copyright-holders:Wilbert Pol,Stefan Jokisch` (MAME as a whole is GPL, which is why only these numbers are used, from this one file, and none of its code). The values are kept with this notice. The licence text below is the standard BSD 3-Clause text with the holders the file header names; MAME's own legal directory (`docs/legal`) was not read.

```
BSD 3-Clause License

Copyright (c) Wilbert Pol, Stefan Jokisch

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are met:

1. Redistributions of source code must retain the above copyright notice, this
   list of conditions and the following disclaimer.

2. Redistributions in binary form must reproduce the above copyright notice,
   this list of conditions and the following disclaimer in the documentation
   and/or other materials provided with the distribution.

3. Neither the name of the copyright holder nor the names of its
   contributors may be used to endorse or promote products derived from
   this software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE
FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
```

## SameBoy `Core/display.c` palette values (the DMG, Pocket and Light colours of the Game Boy target)

`presets/targets/game_boy_lcd.toml` holds, as its variants 3 to 5, the colours of the three palettes `GB_PALETTE_DMG`, `GB_PALETTE_MGB` and `GB_PALETTE_GBL` in `Core/display.c` of SameBoy (<https://github.com/LIJI32/SameBoy>, Copyright (c) 2015-2026 Lior Halphon, the Expat (MIT) licence for every file except the iOS and HexFiend directories, which are not used): DMG 081810, 396139, 84A563, C6DE8C, Pocket 07100E, 3A4C3A, 818D66, C2CE93 and Light 0A1C15, 357862, 56B495, 7FE2C3 (darkest to lightest in the source; the LCD-off colour of each is not used). They are numbers only, no code is copied. SameBoy's author calls the DMG palette 'not faithful' (issue 269); the numbers are an emulator convention, not a measurement.

The same file holds, as its variants 1 and 2, the 'reality' colours of the BGB emulator (closed-source freeware, no licence stated, its author not asked). They are not covered by the notice below; see the notes of the target file.

```
Expat License

Copyright (c) 2015-2026 Lior Halphon

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```
