// Service worker of the web client. It makes the app installable and wakes it up for pushes. The
// push payload is sealed to this device (e2ee.md §7), so until the worker opens it with the
// device's push key it can only say that something arrived, never what.
self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('activate', (event) => event.waitUntil(self.clients.claim()));

self.addEventListener('push', (event) => {
  event.waitUntil(
    self.registration.showNotification('TeamCraft', {
      body: 'New activity',
      tag: 'teamcraft-wake-up',
      renotify: false,
    }),
  );
});

self.addEventListener('notificationclick', (event) => {
  event.notification.close();
  event.waitUntil(
    self.clients.matchAll({ type: 'window', includeUncontrolled: true }).then((windows) => {
      const open = windows.find((client) => 'focus' in client);
      return open ? open.focus() : self.clients.openWindow('/');
    }),
  );
});
