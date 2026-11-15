// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. https://mozilla.org/MPL/2.0/.
(() => {
  const catalog = 'https://raw.githubusercontent.com/amythiz/ipa-archive-updated/main/data/';
  const dialog = document.getElementById('archive');
  const el = (name) => document.getElementById('archive-' + name);
  const results = el('results');
  const pageSize = 24;
  let entries, matches = [], page = 0, loading = false, transfer;
  const imported = new Set();

  function message(value) {
    el('message').textContent = value;
    el('message').hidden = !value;
  }

  async function json(name) {
    const response = await fetch(catalog + name);
    if (!response.ok) throw new Error('HTTP ' + response.status);
    return response.json();
  }

  function osVersion(os) {
    if (!os) return '';
    const parts = [Math.floor(os / 10000), Math.floor(os / 100) % 100];
    if (os % 100) parts.push(os % 100);
    return parts.join('.');
  }

  async function load() {
    if (loading || entries) return;
    loading = true;
    el('retry').hidden = true;
    el('status').textContent = 'Loading archive…';
    message('');
    try {
      const [bases, rows] = await Promise.all([json('urls.json'), json('ipa.json')]);
      entries = [];
      for (const [, device, os, title, bundle, version, collection, path, size] of rows) {
        const base = bases[collection];
        if (!base?.startsWith('https://archive.org/download/') || !path?.toLowerCase().endsWith('.ipa')) continue;
        const minOS = osVersion(os);
        const collectionName = base.slice('https://archive.org/download/'.length);
        entries.push({ device, os, minOS, collectionName, title: title || bundle || path, bundle, version, base, path, size,
          search: [title, bundle, path, collectionName, minOS && 'iphone os ' + minOS,
            minOS && 'ios ' + minOS].join(' ').toLowerCase() });
      }
      const versions = new Set(entries.map(game => game.os).filter(Boolean));
      for (const os of [...versions].sort((a, b) => a - b)) {
        el('os').add(new Option('Min. iOS ' + osVersion(os), String(os)));
      }
      filter();
    } catch (error) {
      entries = undefined;
      el('status').textContent = '';
      message('Could not load the archive. Check your connection and try again.');
      el('retry').hidden = false;
    } finally {
      loading = false;
    }
  }

  function filter() {
    if (!entries) return;
    const device = Number(el('device').value);
    const os = Number(el('os').value);
    const query = el('search').value.trim().toLowerCase();
    const osQuery = /^(?:iphone\s+os|ios|os)\s*(\d+(?:\.\d+){0,2})$/.exec(query);
    matches = entries.filter(game => {
      if ((device && !(game.device & device)) || (os && game.os !== os)) return false;
      if (osQuery) return game.minOS === osQuery[1] || game.minOS.startsWith(osQuery[1] + '.');
      return game.search.includes(query);
    });
    page = 0;
    render();
  }

  function render() {
    results.replaceChildren();
    const start = page * pageSize;
    const end = Math.min(start + pageSize, matches.length);
    el('status').textContent = end ? (start + 1) + '–' + end + ' of ' + matches.length.toLocaleString() : 'No matches';
    el('pages').hidden = matches.length <= pageSize;
    el('prev').disabled = !page;
    el('next').disabled = end === matches.length;
    for (const game of matches.slice(start, end)) {
      const row = document.createElement('div');
      row.className = 'archive-result';
      const info = document.createElement('div');
      const title = document.createElement('strong');
      title.textContent = game.title;
      const meta = document.createElement('small');
      meta.className = 'archive-meta';
      meta.textContent = [game.version && 'v' + game.version, game.minOS && 'iOS ' + game.minOS + '+',
        game.size && (game.size / 1024).toFixed(1) + ' MB'].filter(Boolean).join(' · ');
      const collection = document.createElement('small');
      collection.className = 'archive-meta';
      collection.textContent = game.collectionName;
      info.append(title, meta, collection);
      const button = document.createElement('button');
      button.className = 'small';
      button.textContent = imported.has(game) ? 'Added' : 'Import';
      button.disabled = !!transfer || imported.has(game);
      button.addEventListener('click', () => importGame(game));
      row.append(info, button);
      results.append(row);
    }
  }

  async function importGame(game) {
    if (transfer) return;
    const controller = new AbortController();
    transfer = controller;
    render();
    message('Downloading ' + game.title + '…');
    try {
      const path = game.path.split('/').map(encodeURIComponent).join('/');
      const url = game.base.replace('/download/', '/cors/') + '/' + path;
      const response = await fetch(url, { signal: controller.signal, credentials: 'omit' });
      if (!response.ok) throw new Error('Archive returned HTTP ' + response.status + '.');
      const file = new File([await response.blob()], game.path.split('/').pop());
      if (!(await zipEntries(file)).some(entry => /^Payload\/[^/]+\.app\/Info\.plist$/.test(entry.name))) {
        throw new Error('The download did not contain an IPA app bundle.');
      }
      if (controller.signal.aborted) return;
      message('Adding ' + game.title + ' to your library…');
      await keepGame(file, false);
      imported.add(game);
      message(game.title + ' was added to your library.');
    } catch (error) {
      if (!controller.signal.aborted) {
        message(error.name === 'ConstraintError' ? 'A game with this filename is already in your library.' :
          'Could not import ' + game.title + '. ' + error.message);
      }
    } finally {
      transfer = null;
      render();
    }
  }

  document.getElementById('browse-archive').addEventListener('click', () => {
    dialog.showModal();
    if (!transfer) { imported.clear(); message(''); }
    if (entries) filter(); else load();
  });
  el('close').onclick = () => dialog.close();
  dialog.addEventListener('close', () => transfer?.abort());
  el('retry').onclick = load;
  el('device').onchange = filter;
  el('os').onchange = filter;
  el('search').oninput = filter;
  el('prev').onclick = () => { page--; render(); results.scrollTop = 0; };
  el('next').onclick = () => { page++; render(); results.scrollTop = 0; };
})();
