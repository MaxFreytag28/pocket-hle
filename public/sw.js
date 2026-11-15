/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
// Service worker for the web build: makes the page work offline once it has
// been loaded.
//
// The strategy is "network first": while online, everything comes from the
// network as usual (so an updated build is picked up straight away) and a
// copy is kept. Offline, the copy is used.

const CACHE = 'pocket-hle-v1';

self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('activate', (event) => {
  event.waitUntil((async () => {
    for (const name of await caches.keys()) {
      if (name !== CACHE) await caches.delete(name);
    }
    await self.clients.claim();
  })());
});

self.addEventListener('fetch', (event) => {
  const request = event.request;
  const url = new URL(request.url);
  // Only the page's own files. App files are never fetched from the network
  // in normal use, and must not end up in this cache.
  if (request.method !== 'GET' || url.origin !== self.location.origin || url.pathname.endsWith('.ipa')) {
    return;
  }
  event.respondWith((async () => {
    const cache = await caches.open(CACHE);
    try {
      const response = await fetch(request);
      if (response.ok) cache.put(request, response.clone());
      return response;
    } catch (error) {
      const cached = await cache.match(request, { ignoreSearch: true });
      if (cached) return cached;
      throw error;
    }
  })());
});
