# Third-party notices

The application bundles or uses third-party components. Their own licenses remain in effect; this notice does not replace the upstream license texts.

| Component | Use | License/source |
| --- | --- | --- |
| aria2 | RPC download engine bundled in platform Release packages | [aria2 releases](https://github.com/aria2/aria2/releases), GPL-2.0-or-later |
| FFmpeg | Media merge, recording and subtitle rendering | [FFmpeg](https://ffmpeg.org/), LGPL/GPL depending on the build configuration |
| DB-IP Country Lite | Optional GeoIP country database | [DB-IP](https://db-ip.com/db/download/ip-to-country-lite), CC BY 4.0 |
| Font Awesome Free | Frontend icons | [Font Awesome](https://fontawesome.com/), CC BY 4.0 / SIL OFL 1.1 / MIT as applicable |
| qrcode.js | QR code rendering | [qrcode.js](https://github.com/davidshimjs/qrcodejs), MIT |
| Socket.IO client | Frontend realtime transport | [Socket.IO](https://socket.io/), MIT |

Exact bundled versions and SHA-256 values are listed in [`resources/README.md`](resources/README.md). The Unix runtime binaries are copied from the official CI runner packages at release-build time and receive per-package `.sha256` manifests.
