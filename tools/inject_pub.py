# Создаёт ключ подписи интерфейса (если нет) и вшивает открытый ключ в ui/hot.js
import os,base64,re
from cryptography.hazmat.primitives.asymmetric import ec
from cryptography.hazmat.primitives import serialization
p='/root/.tauri/legends-island-ui.pem'
if not os.path.exists(p):
    k=ec.generate_private_key(ec.SECP256R1())
    open(p,'wb').write(k.private_bytes(serialization.Encoding.PEM,serialization.PrivateFormat.PKCS8,serialization.NoEncryption())); os.chmod(p,0o600)
k=serialization.load_pem_private_key(open(p,'rb').read(),None)
n=k.public_key().public_numbers()
b=lambda i: base64.urlsafe_b64encode(i.to_bytes(32,'big')).decode().rstrip('=')
jwk='{kty:"EC",crv:"P-256",x:"%s",y:"%s"}'%(b(n.x),b(n.y))
h=open('ui/hot.js').read()
h=re.sub(r'var LI_PUB=.*?;\n','var LI_PUB=%s;\n'%jwk,h,count=1)
open('ui/hot.js','w').write(h)
