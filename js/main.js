(() => {
  'use strict';
  const menu = document.getElementById('menu-toggle');
  const nav = document.getElementById('primary-nav');
  menu?.addEventListener('click', () => {
    const isOpen = nav.classList.toggle('open');
    menu.setAttribute('aria-expanded', String(isOpen));
    menu.setAttribute('aria-label', isOpen ? 'Закрыть меню' : 'Открыть меню');
  });
  nav?.querySelectorAll('a').forEach(link => link.addEventListener('click', () => {
    nav.classList.remove('open');
    menu?.setAttribute('aria-expanded', 'false');
  }));
  document.addEventListener('click', (event) => {
    if (nav?.classList.contains('open') && !nav.contains(event.target) && !menu?.contains(event.target)) {
      nav.classList.remove('open');
      menu?.setAttribute('aria-expanded', 'false');
    }
  });

  const reveals = document.querySelectorAll('[data-reveal]');
  if ('IntersectionObserver' in window) {
    const observer = new IntersectionObserver(entries => entries.forEach(entry => {
      if (entry.isIntersecting) { entry.target.classList.add('visible'); observer.unobserve(entry.target); }
    }), { threshold: 0.1, rootMargin: '0px 0px 50px 0px' });
    reveals.forEach(el => observer.observe(el));
  } else {
    reveals.forEach(el => el.classList.add('visible'));
  }

  const slides = [
    { id:'home', title:'Главный экран — активные сервисы и проверка доступа' },
    { id:'services', title:'Сервисы — управление DNS, Telegram и обходом' },
    { id:'speed', title:'Диагностика — замеры скорости и задержки' },
    { id:'settings', title:'Настройки — компоненты, обновления и поведение приложения' },
    { id:'diagnostics', title:'Проверка системы — драйверы и конфликты' },
    { id:'home-start', title:'Запуск обхода — проверка подключения' }
  ];
  const dialog = document.getElementById('lightbox');
  const modalImage = document.getElementById('lightbox-image');
  const caption = document.getElementById('lightbox-caption');
  let current = 0;
  let opener = null;
  function showSlide(n) {
    current = (n + slides.length) % slides.length;
    modalImage.src = `assets/${slides[current].id}.webp`;
    modalImage.alt = slides[current].title;
    caption.textContent = `${slides[current].title} · ${current+1} / ${slides.length}`;
  }
  document.querySelectorAll('[data-shot]').forEach(button => button.addEventListener('click', () => {
    opener = button;
    const i = slides.findIndex(s => s.id === button.dataset.shot);
    showSlide(i < 0 ? 0 : i);
    dialog.showModal();
    document.body.classList.add('dialog-open');
  }));
  function close() { if (dialog.open) dialog.close(); }
  document.getElementById('lightbox-close')?.addEventListener('click', close);
  document.getElementById('lightbox-prev')?.addEventListener('click', () => showSlide(current-1));
  document.getElementById('lightbox-next')?.addEventListener('click', () => showSlide(current+1));
  dialog?.addEventListener('click', e => { if (e.target === dialog) close(); });
  dialog?.addEventListener('close', () => { document.body.classList.remove('dialog-open'); opener?.focus(); });
  document.addEventListener('keydown', e => {
    if (!dialog?.open) return;
    if (e.key === 'ArrowLeft') showSlide(current-1);
    if (e.key === 'ArrowRight') showSlide(current+1);
  });
})();
