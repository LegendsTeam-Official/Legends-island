#!/bin/bash
# Сборка Windows (.exe) и Linux (AppImage + .deb), выкладка обновления для обеих систем
# ./release-all.sh "что нового"
set -e
cd "$(dirname "$0")"
export PATH="$HOME/.cargo/bin:$PATH"
KEY=/root/.tauri/legends-island.key
WWW=/var/www/legends-island
NOTES="${1:-}"
./build.sh                       # Windows: подписанный NSIS-установщик
export TAURI_SIGNING_PRIVATE_KEY="$(cat $KEY)" TAURI_SIGNING_PRIVATE_KEY_PASSWORD="${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:?задай пароль ключа подписи}" APPIMAGE_EXTRACT_AND_RUN=1 NO_STRIP=true
nice -n 15 npx tauri build --bundles appimage,deb   # Linux
VER=$(python3 -c "import json;print(json.load(open('src-tauri/tauri.conf.json'))['version'])")
WEXE="src-tauri/target/x86_64-pc-windows-msvc/release/bundle/nsis/Legends Island_${VER}_x64-setup.exe"
LAPP="src-tauri/target/release/bundle/appimage/Legends Island_${VER}_amd64.AppImage"
LDEB="src-tauri/target/release/bundle/deb/Legends Island_${VER}_amd64.deb"
mkdir -p $WWW
cp "$WEXE" "$WWW/LegendsIsland_${VER}_x64-setup.exe"; cp "$WEXE" "$WWW/LegendsIsland-setup.exe"
cp "$LAPP" "$WWW/LegendsIsland_${VER}_amd64.AppImage"; cp "$LAPP" "$WWW/LegendsIsland-x86_64.AppImage"
cp "$LDEB" "$WWW/LegendsIsland_amd64.deb"
python3 - "$VER" "$(cat "$WEXE.sig")" "$(cat "$LAPP.sig")" "$NOTES" <<'PY'
import json,sys,datetime
v,ws,ls,notes=sys.argv[1:5]
b="https://island.lgscode.online/"
json.dump({"version":v,"notes":notes or f"Legends Island {v}","pub_date":datetime.datetime.now(datetime.timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ'),
 "platforms":{"windows-x86_64":{"signature":ws,"url":b+f"LegendsIsland_{v}_x64-setup.exe"},
              "linux-x86_64":{"signature":ls,"url":b+f"LegendsIsland_{v}_amd64.AppImage"}}},
 open('/var/www/legends-island/latest.json','w'),ensure_ascii=False,indent=2)
PY
./publish-ui.sh
ls -la $WWW | grep -E "setup|AppImage|deb|latest"
