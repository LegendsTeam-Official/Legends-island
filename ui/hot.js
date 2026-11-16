// Горячее обновление интерфейса: берёт подписанный UI с сервера, иначе — сохранённый, иначе — встроенный в приложение.
// Открытый ключ вшит при сборке; без верной подписи чужой интерфейс не загрузится.
var LI_BASE='https://island.lgscode.online/ui/v4/';
var LI_PUB={kty:"EC",crv:"P-256",x:"jXB4da9iA9O69Vm3AmdyNeotwYzvo8cDBUFskxEW27E",y:"q2eJoHXZY9XQ7rQyoirp0heNAfcJ5Bw2Y8fp_QN9f_8"};
async function liVerify(text,sig){
  try{ const k=await crypto.subtle.importKey('jwk',LI_PUB,{name:'ECDSA',namedCurve:'P-256'},false,['verify']);
    return await crypto.subtle.verify({name:'ECDSA',hash:'SHA-256'},k,Uint8Array.from(atob(sig),c=>c.charCodeAt(0)),new TextEncoder().encode(text)); }
  catch(e){ return false }
}
async function liFetch(url,ms){
  const c=new AbortController(), t=setTimeout(()=>c.abort(),ms);
  try{ const r=await fetch(url,{signal:c.signal,cache:'no-store'}); return r.ok?r:null }catch(e){ return null } finally{ clearTimeout(t) }
}
function liGet(n){ try{ return JSON.parse(localStorage.getItem('li-ui:'+n)) }catch(e){ return null } }
function liSet(n,v){ try{ localStorage.setItem('li-ui:'+n,JSON.stringify(v)) }catch(e){} }
async function liRemote(name){
  const m=await liFetch(LI_BASE+'manifest.json?t='+Date.now(),2500); if(!m) return null;
  let man; try{ man=await m.json() }catch(e){ return null }
  const sig=man.files&&man.files[name]; if(!sig) return null;
  const st=liGet(name); if(st&&st.sig===sig) return st;
  const r=await liFetch(LI_BASE+name+'?s='+encodeURIComponent(sig.slice(0,12)),5000); if(!r) return null;
  const html=await r.text();
  return (await liVerify(html,sig)) ? {html,sig} : null;
}
async function boot(name){
  let ui=await liRemote(name);
  if(ui) liSet(name,ui);
  else { const st=liGet(name); if(st && await liVerify(st.html,st.sig)) ui=st; }
  const html = ui ? ui.html : await (await fetch(name)).text();
  window.__LI_SIG = ui ? ui.sig : 'local';
  document.open(); document.write(html); document.close();
}
function watchUi(name){
  setInterval(async()=>{
    if(window.liBusy && window.liBusy()) return;
    const ui=await liRemote(name);
    if(ui && ui.sig!==window.__LI_SIG){ liSet(name,ui); location.reload(); }
  }, 10*60*1000);
}
