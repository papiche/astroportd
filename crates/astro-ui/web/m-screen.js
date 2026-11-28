'use strict';
// Écran d'un navigateur ouvert par la station (navigateur prêté à un agent, navigation partagée avec un ami) : les images de la
// page arrivent en flux, les gestes repartent. Trois modes : « drive » (cliquer, taper), « point » (toucher montre un endroit
// à tous, sans cliquer), « watch » (regarder). Les pointeurs des autres s'affichent par-dessus la page.
const css = document.createElement('link');
css.rel = 'stylesheet'; css.href = '/ui/m-screen.css'; document.head.append(css);

export function viewer(K, { stream, send, mode = 'drive' }) {
  const { h, t } = K;
  const img = h('img', { class: 'ag-screen', alt: t('Page partagée') }), note = h('p', { class: 'muted', role: 'status' }, t('Ouverture du navigateur…'));
  const dots = h('div', { class: 'sc-dots', 'aria-hidden': 'true' }), frame = h('div', { class: 'sc-frame' }, img, dots);
  const post = g => send(g).catch(e => K.toast(e.message));
  const at = ev => { const r = img.getBoundingClientRect(); return { x: Math.min(1, Math.max(0, (ev.clientX - r.left) / r.width)), y: Math.min(1, Math.max(0, (ev.clientY - r.top) / r.height)) }; };
  img.onclick = ev => { if (mode === 'drive') post({ type: 'click', ...at(ev) }); else if (mode === 'point') post({ type: 'point', ...at(ev) }); };
  let wheel = 0, wt = null;
  img.onwheel = ev => { if (mode !== 'drive') return; ev.preventDefault(); wheel += ev.deltaY; const p = at(ev); clearTimeout(wt); wt = setTimeout(() => { post({ type: 'scroll', ...p, dy: wheel }); wheel = 0; }, 80); };
  const typing = h('input', { secret: true, class: 'ag-type', placeholder: t('Taper ici'), 'aria-label': t('Taper dans la page') });
  let tt = null;
  const flush = async () => { clearTimeout(tt); const v = typing.value; typing.value = ''; if (v) await post({ type: 'text', text: v }); };
  typing.oninput = () => { clearTimeout(tt); tt = setTimeout(flush, 400); };
  typing.onkeydown = ev => {
    if (!['Enter', 'Tab', 'Backspace', 'Escape', 'Delete', 'ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown'].includes(ev.key)) return;
    if (ev.key === 'Backspace' && typing.value) return; // efface d'abord ce qui n'est pas encore parti
    ev.preventDefault(); flush().then(() => post({ type: 'key', key: ev.key }));
  };
  const eye = h('button', { type: 'button', class: 'ghost', 'aria-label': t('Montrer ce que je tape'), onclick: () => typing.classList.toggle('secret') }, '👁');
  const key = (k, label, aria) => h('button', { type: 'button', class: 'ghost', 'aria-label': aria, onclick: () => flush().then(() => post({ type: 'key', key: k })) }, label);
  const scroll = (dy, label, aria) => h('button', { type: 'button', class: 'ghost', 'aria-label': aria, onclick: () => post({ type: 'scroll', x: 0.5, y: 0.5, dy }) }, label);
  const tools = h('div', { class: 'ag-row' }, typing, eye, key('Backspace', '⌫', t('Effacer')), key('Tab', '⇥', t('Champ suivant')), key('Enter', '⏎', t('Valider')), scroll(-500, '▲', t('Monter')), scroll(500, '▼', t('Descendre')));
  const hint = h('small', { class: 'muted' });
  const setMode = m => {
    mode = m; tools.hidden = m !== 'drive'; img.classList.toggle('sc-still', m === 'watch');
    hint.textContent = m === 'drive' ? t('Touchez la page pour cliquer. Ce que vous tapez part directement dans le champ choisi.') : m === 'point' ? t('Touchez la page pour montrer un endroit.') : t('Vous regardez la page.');
  };
  setMode(mode);
  // Pointeurs des autres : un point nommé par personne, effacé après 6 s sans mouvement.
  const marks = new Map();
  const pointer = p => {
    let m = marks.get(p.hex);
    if (!m) { m = h('span', { class: 'sc-dot' }, h('span', {}, p.who)); marks.set(p.hex, m); dots.append(m); }
    m.style.setProperty('--x', p.x); m.style.setProperty('--y', p.y);
    clearTimeout(m._t); m._t = setTimeout(() => { m.remove(); marks.delete(p.hex); }, 6000);
  };
  const el = h('div', { class: 'ag-view' }, note, frame, tools, hint);
  let rd = null, url = null, stopped = false;
  const show = f => {
    const bin = Uint8Array.from(atob(f.jpeg), c => c.charCodeAt(0)), u = URL.createObjectURL(new Blob([bin], { type: 'image/jpeg' }));
    img.src = u; if (url) URL.revokeObjectURL(url); url = u; note.hidden = true;
  };
  const handlers = { frame: show, pointer };
  const ended = (async () => {
    try {
      const r = await K.req(stream);
      if (!r.ok) throw new Error((await r.json().catch(() => ({}))).error || r.statusText);
      rd = r.body.pipeThrough(new TextDecoderStream()).getReader();
      let buf = '';
      for (;;) {
        const { value, done } = await rd.read();
        if (done) break;
        buf += value;
        for (let i; (i = buf.indexOf('\n\n')) >= 0;) {
          const blk = buf.slice(0, i), ev = (blk.match(/^event: ?(\w+)$/m) || [])[1], data = blk.split('\n').filter(l => l.startsWith('data:')).map(l => l.slice(5).trimStart()).join('');
          buf = buf.slice(i + 2);
          if (ev && data) { try { (handlers[ev] || out.on[ev] || (() => {}))(JSON.parse(data)); } catch { /* image incomplète */ } }
        }
      }
    } catch (e) { if (!stopped) { note.hidden = false; note.textContent = e.message; } }
  })();
  const out = { el, setMode, ended, on: {}, stop: () => { stopped = true; rd?.cancel().catch(() => {}); if (url) URL.revokeObjectURL(url); } };
  return out;
}
