#!/bin/bash
# Выкладывает интерфейс (ui/*-app.html) на сервер с подписью — приложение подхватывает его без обновления и переустановки
set -e
cd "$(dirname "$0")"
KEY=/root/.tauri/legends-island-ui.pem
WWW=/var/www/legends-island/ui/v4
mkdir -p $WWW
python3 - "$KEY" "$WWW" <<'PY'
import sys,os,json,base64,datetime
from cryptography.hazmat.primitives.asymmetric import ec, utils
from cryptography.hazmat.primitives import hashes, serialization
key_path,www=sys.argv[1],sys.argv[2]
if not os.path.exists(key_path):
    k=ec.generate_private_key(ec.SECP256R1())
    open(key_path,'wb').write(k.private_bytes(serialization.Encoding.PEM,serialization.PrivateFormat.PKCS8,serialization.NoEncryption()))
    os.chmod(key_path,0o600)
k=serialization.load_pem_private_key(open(key_path,'rb').read(),None)
files={}
for n in sorted(os.listdir('ui')):
    if not n.endswith('-app.html'): continue
    data=open('ui/'+n,'rb').read()
    r,s=utils.decode_dss_signature(k.sign(data,ec.ECDSA(hashes.SHA256())))
    files[n]=base64.b64encode(r.to_bytes(32,'big')+s.to_bytes(32,'big')).decode()
    open(os.path.join(www,n),'wb').write(data)
json.dump({"updated":datetime.datetime.utcnow().strftime('%Y-%m-%dT%H:%M:%SZ'),"files":files},open(os.path.join(www,'manifest.json'),'w'),indent=2)
print('UI выложен:',', '.join(files))
PY
