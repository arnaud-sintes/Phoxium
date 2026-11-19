# Third-party notices

Upstream licence notices of third-party code built into ultimateCRT that the crate packages do not carry
themselves. `scripts/package.ps1` appends this file to the generated `THIRD-PARTY.md` of the release zip (after
the table of every crate and its declared licence, from `cargo metadata`). The licence policy (permissive only,
no GPL) is checked by `cargo deny check licenses` with `deny.toml`.

## Dear ImGui (through `dear-imgui-sys`, `dear-imgui-rs`, `dear-imgui-ash` 0.22.0)

The settings menu and the stats overlay (T-52a) use Dear ImGui 1.93.1 WIP (docking branch) through cimgui,
vendored and compiled from source by `dear-imgui-sys` (C++ with MSVC). The three `dear-imgui-*` crates declare
`MIT OR Apache-2.0` in their manifests but their packages ship no LICENSE file (checked at T-08); their
repository is <https://github.com/Latias94/dear-imgui-rs>. The vendored C and C++ sources carry the notices
below (copied from the `dear-imgui-sys` 0.22.0 package). The stack-layout shim of `dear-imgui-sys`
(`THIRD_PARTY_NOTICES.md` of that package, MIT) is compiled only with its `stack-layout` feature, which
ultimateCRT does not enable.

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
