#!/bin/bash
# Сборка Legends Island под Windows на этом сервере + выкладка обновления
# Использование: ./build.sh            — собрать
#                ./build.sh release    — собрать и выложить на island.lgscode.online
set -e
cd "$(dirname "$0")"
export PATH="$HOME/.cargo/bin:$PATH"
KEY=/root/.tauri/legends-island.key
WWW=/var/www/legends-island
[ -d node_modules ] || npm i --no-audit --no-fund
mkdir -p ui/fonts
cp node_modules/@fontsource/lora/files/lora-cyrillic-600-normal.woff2 node_modules/@fontsource/lora/files/lora-latin-600-normal.woff2 ui/fonts/
[ -f src-tauri/icons/icon.ico ] || npx tauri icon src-tauri/app-icon.png >/dev/null
cp src-tauri/icons/128x128.png ui/icon.png
if [ ! -f "$KEY" ]; then mkdir -p /root/.tauri; npx tauri signer generate --ci -w "$KEY" >/dev/null; fi
PUB=$(cat "$KEY.pub")
python3 - "$PUB" <<'PY'
import json,sys
p='src-tauri/tauri.conf.json'; c=json.load(open(p)); c['plugins']['updater']['pubkey']=sys.argv[1]; json.dump(c,open(p,'w'),ensure_ascii=False,indent=2)
PY
export TAURI_SIGNING_PRIVATE_KEY="$(cat $KEY)" TAURI_SIGNING_PRIVATE_KEY_PASSWORD="${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:?задай пароль ключа подписи}"
python3 tools/inject_pub.py
nice -n 15 npx tauri build --runner cargo-xwin --target x86_64-pc-windows-msvc
VER=$(python3 -c "import json;print(json.load(open('src-tauri/tauri.conf.json'))['version'])")
OUT=src-tauri/target/x86_64-pc-windows-msvc/release/bundle/nsis
EXE="$OUT/Legends Island_${VER}_x64-setup.exe"
ls -la "$OUT"
if [ "$1" = "release" ]; then
  mkdir -p $WWW
  cp "$EXE" "$WWW/LegendsIsland_${VER}_x64-setup.exe"
  cp "$EXE" "$WWW/LegendsIsland-setup.exe"
  python3 - "$VER" "$(cat "$EXE.sig")" "${2:-}" <<'PY'
import json,sys,datetime
v,sig,notes=sys.argv[1],sys.argv[2],sys.argv[3]
json.dump({"version":v,"notes":notes or f"Legends Island {v}","pub_date":datetime.datetime.now(datetime.timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ'),
 "platforms":{"windows-x86_64":{"signature":sig,"url":f"https://island.lgscode.online/LegendsIsland_{v}_x64-setup.exe"}}},open('/var/www/legends-island/latest.json','w'),ensure_ascii=False,indent=2)
PY
  ./publish-ui.sh
  echo "Выложено: https://island.lgscode.online/latest.json (версия $VER)"
fi
