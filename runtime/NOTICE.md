# Pi Runtime Notice

Mira remains GPL-3.0. Its runtime includes `@earendil-works/pi-ai` and
`@earendil-works/pi-telemetry`, from <https://github.com/earendil-works/pi>.
Their MIT notice is reproduced below.

## Pi MIT License

Copyright (c) 2025 Mario Zechner

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

## Standalone Runtime

The executable embeds Bun and the dependencies pinned in `pnpm-lock.yaml`.
Bun's own and linked-library licensing information is maintained at
<https://github.com/oven-sh/bun/blob/main/LICENSE.md>, including JavaScriptCore's
LGPL terms and rebuild/relink instructions. The complete Mira runtime source
and its reproducible build entry point (`runtime/scripts/build.mjs`) are in
this repository. This Pi notice is not a complete transitive-license audit;
review third-party license obligations before distributing releases.
