<script lang="ts">
    import { onMount } from 'svelte';
    import { listen } from '@tauri-apps/api/event';
    import { save, open } from '@tauri-apps/plugin-dialog';
    import { invoke } from '@tauri-apps/api/core';
    import QRCode from 'qrcode';
    import { fly, fade } from 'svelte/transition';

    let activeTab = 'devices';
    let qrMode = 'receive';
    let qrCode = '---';
    let qrDataUrl = '';
    let localIp = '...';
    let qrError = '';
    let devices: any[] = [];
    let updateInterval: any = null;
    let theme = 'system';
    let manualIp = '';
    let pendingFile: any = null;
    let saveStatus = '';
    let sendStatus = '';
    let selectedFileName = '';
    let isDragging = false;
    let toasts: Array<{id: number, message: string, type: string}> = [];
    let toastId = 0;
    let transferProgress: {filename: string, percent: number, visible: boolean} = {filename: '', percent: 0, visible: false};

    function showToast(message: string, type: string = 'info') {
        const id = ++toastId;
        toasts = [...toasts, { id, message, type }];
        setTimeout(() => { toasts = toasts.filter(t => t.id !== id); }, 3000);
    }

    let saveTimeout: any;
    $: { clearTimeout(saveTimeout); saveTimeout = setTimeout(async () => { try { await invoke('update_settings', { theme, manualIp }); } catch (e) {} }, 500); }

    // ВАЖНО: Это reactive statement - вызывается при каждом изменении theme
    $: applyTheme(theme);

    function applyTheme(t: string) {
        const root = document.documentElement;
        const isDark = t === 'dark' || (t === 'system' && window.matchMedia('(prefers-color-scheme: dark)').matches);
        if (isDark) { 
            root.setAttribute('data-theme', 'dark'); 
            document.body.className = 'theme-dark';
        } else { 
            root.setAttribute('data-theme', 'light'); 
            document.body.className = 'theme-light';
        }
    }

    async function loadData() {
        try {
            const settings = await invoke<any>('get_settings');
            // Загружаем настройки только при первом запуске
            if (!theme || theme === 'system') {
                theme = settings.theme || 'system';
                manualIp = settings.manual_ip || '';
            }
        } catch (e) { console.error("Settings error:", e); }
        
        try {
            const res = await fetch('http://127.0.0.1:7431/api/status');
            const data = await res.json();
            qrCode = data.code || '---';
            localIp = data.local_ip || '127.0.0.1';
            if (qrCode !== '---' && localIp && localIp !== '127.0.0.1') { generateQR(); }
        } catch (e) { qrError = 'Сервер недоступен'; }
        try { devices = await invoke<any[]>('get_devices'); } catch (e) { console.error("Devices error:", e); }
    }

    async function generateQR() {
        if (qrCode === '---' || !localIp || localIp === '127.0.0.1') return;
        if (qrMode === 'send' && qrDataUrl) return;
        const isDark = theme === 'dark' || (theme === 'system' && window.matchMedia('(prefers-color-scheme: dark)').matches);
        if (qrMode === 'receive') {
            qrDataUrl = await QRCode.toDataURL(`http://${localIp}:7431?code=${qrCode}`, { width: 220, margin: 2, color: { dark: isDark ? '#ffffff' : '#0f172a', light: isDark ? '#0f172a' : '#ffffff' }, errorCorrectionLevel: 'M' });
        }
        qrError = '';
    }

    async function selectFileForSend(filePath?: string) {
        sendStatus = 'Подготовка файла...';
        const targetPath = filePath || await open({ multiple: false });
        if (!targetPath) { sendStatus = ''; return; }
        try {
            const token = await invoke('prepare_file_for_download', { filePath: targetPath });
            selectedFileName = targetPath.split('\\').pop() || targetPath.split('/').pop() || 'file';
            const isDark = theme === 'dark' || (theme === 'system' && window.matchMedia('(prefers-color-scheme: dark)').matches);
            qrDataUrl = await QRCode.toDataURL(`http://${localIp}:7431/download?token=${token}`, { width: 220, margin: 2, color: { dark: isDark ? '#ffffff' : '#0f172a', light: isDark ? '#0f172a' : '#ffffff' }, errorCorrectionLevel: 'M' });
            sendStatus = `Готово. Отсканируйте QR-код телефоном.`;
            showToast('Файл готов к отправке', 'success');
        } catch (err) { sendStatus = 'Ошибка: ' + err; selectedFileName = ''; qrDataUrl = ''; showToast('Ошибка подготовки файла', 'error'); }
    }

    function handleDrop(e: DragEvent) { e.preventDefault(); isDragging = false; if (e.dataTransfer?.files.length) { selectFileForSend(); } }

    async function sendFileToDevice(targetIp: string) {
        const selected = await open({ multiple: false });
        if (!selected) { return; }
        transferProgress = { filename: selected.split('\\').pop() || 'file', percent: 0, visible: true };
        try {
            await invoke('send_file_to_device', { targetIp, filePath: selected });
            showToast('Файл успешно отправлен!', 'success');
            transferProgress.visible = false;
        } catch (err: any) { showToast('Ошибка: ' + err.message, 'error'); transferProgress.visible = false; }
    }

    onMount(async () => {
        await loadData();
        updateInterval = setInterval(loadData, 3000);
        const unlistenFile = await listen('file_received', (event: any) => { pendingFile = event.payload; saveStatus = ''; showToast('Получен новый файл', 'success'); });
        const unlistenTransfer = await listen('transfer-complete', (event: any) => {
            if (event.payload.status === 'success') { showToast(event.payload.message, 'success'); }
            else { showToast('Ошибка: ' + event.payload.message, 'error'); }
            transferProgress.visible = false;
        });
        return () => { clearInterval(updateInterval); unlistenFile(); unlistenTransfer(); };
    });

    async function handleSave() {
        if (!pendingFile) return;
        saveStatus = 'Открытие диалога...';
        try {
            const filePath = await save({ defaultPath: pendingFile.filename });
            if (filePath) {
                saveStatus = 'Сохранение...';
                const result = await invoke<string>('save_incoming_file', { tempPath: pendingFile.temp_path, destPath: filePath });
                saveStatus = 'Успешно сохранено';
                showToast('Файл сохранен', 'success');
                setTimeout(() => { pendingFile = null; saveStatus = ''; }, 2000);
            } else { saveStatus = 'Отменено'; pendingFile = null; }
        } catch (err) { saveStatus = 'Ошибка: ' + err; }
    }
</script>

<svelte:head>
    <style>
        :global(:root) { --transition-speed: 0.3s; --font-sans: 'Segoe UI Variable', 'Segoe UI', system-ui, -apple-system, sans-serif; --font-mono: 'Cascadia Code', 'Fira Code', Consolas, monospace; }
        :global(.theme-light) { --bg-app: #f8fafc; --bg-panel: #ffffff; --bg-panel-hover: #f1f5f9; --text-main: #0f172a; --text-muted: #64748b; --border-color: #e2e8f0; --primary: #6366f1; --primary-hover: #4f46e5; --primary-glow: rgba(99, 102, 241, 0.4); --success: #10b981; --error: #ef4444; --shadow-sm: 0 1px 2px 0 rgb(0 0 0 / 0.05); --shadow-md: 0 4px 6px -1px rgb(0 0 0 / 0.1), 0 2px 4px -2px rgb(0 0 0 / 0.1); --shadow-lg: 0 10px 15px -3px rgb(0 0 0 / 0.1), 0 4px 6px -4px rgb(0 0 0 / 0.1); }
        :global(.theme-dark) { --bg-app: #0f172a; --bg-panel: #1e293b; --bg-panel-hover: #334155; --text-main: #f8fafc; --text-muted: #94a3b8; --border-color: #334155; --primary: #6366f1; --primary-hover: #4f46e5; --primary-glow: rgba(99, 102, 241, 0.4); --success: #10b981; --error: #ef4444; --shadow-md: 0 4px 6px -1px rgb(0 0 0 / 0.3), 0 2px 4px -2px rgb(0 0 0 / 0.3); --shadow-lg: 0 10px 15px -3px rgb(0 0 0 / 0.5), 0 4px 6px -4px rgb(0 0 0 / 0.5); }
        :global(body) { margin: 0; background: var(--bg-app); color: var(--text-main); font-family: var(--font-sans); overflow: hidden; transition: background-color var(--transition-speed) ease, color var(--transition-speed) ease; }
    </style>
</svelte:head>

<div class="app-layout">
    <aside class="sidebar">
        <div class="logo-section">
            <div class="logo-icon">
                <svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                    <path d="M4 12v8a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-8"/>
                    <polyline points="16 6 12 2 8 6"/>
                    <line x1="12" y1="2" x2="12" y2="15"/>
                </svg>
            </div>
            <h1>FileHop</h1>
        </div>
        <nav class="nav-menu">
            <button class:active={activeTab === 'devices'} on:click={() => activeTab = 'devices'}>
                <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                    <rect x="2" y="3" width="20" height="14" rx="2" ry="2"/><line x1="8" y1="21" x2="16" y2="21"/><line x1="12" y1="17" x2="12" y2="21"/>
                </svg>
                <span>Устройства</span>
            </button>
            <button class:active={activeTab === 'qr'} on:click={() => activeTab = 'qr'}>
                <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                    <rect x="3" y="3" width="7" height="7"/><rect x="14" y="3" width="7" height="7"/><rect x="14" y="14" width="7" height="7"/><rect x="3" y="14" width="7" height="7"/>
                </svg>
                <span>QR-код</span>
            </button>
            <button class:active={activeTab === 'settings'} on:click={() => activeTab = 'settings'}>
                <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                    <circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1 0 2.83 2 2 0 0 1-2.83 0l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-2 2 2 2 0 0 1-2-2v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83 0 2 2 0 0 1 0-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1-2-2 2 2 0 0 1 2-2h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 0-2.83 2 2 0 0 1 2.83 0l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 2-2 2 2 0 0 1 2 2v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 0 2 2 0 0 1 0 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 2 2 2 2 0 0 1-2 2h-.09a1.65 1.65 0 0 0-1.51 1z"/>
                </svg>
                <span>Настройки</span>
            </button>
        </nav>
    </aside>

    <main class="main-content">
        {#if activeTab === 'devices'}
            <div class="panel" in:fade={{ duration: 200 }}>
                <h2>Доступные устройства в сети</h2>
                {#if devices.length === 0}
                    <div class="empty-state" in:fade={{ duration: 300 }}>
                        <div class="empty-icon"><svg width="48" height="48" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><path d="M12 8v4M12 16h.01"/></svg></div>
                        <p class="empty-title">Поиск устройств...</p>
                        <p class="empty-subtitle">Убедитесь, что FileHop запущен на целевом ПК</p>
                        <details class="troubleshoot"><summary>Не видите нужное устройство?</summary><ul><li>Убедитесь, что FileHop запущен на целевом ПК.</li><li>Проверьте, что сеть в Windows настроена как <strong>Частная</strong> (Параметры → Сеть и Интернет → Свойства).</li><li>Разрешите FileHop в Брандмауэре Защитника Windows.</li></ul></details>
                    </div>
                {:else}
                    <ul class="device-list">{#each devices as device (device.ip)}<li class="device-item" in:fly={{ y: 10, duration: 200 }}><div class="device-info"><div class="device-header"><span class="device-name">{device.name}</span><div class="status-dot" title="Устройство доступно"></div></div><span class="device-ip">{device.ip}</span></div><button class="btn-primary" on:click={() => sendFileToDevice(device.ip)} disabled={transferProgress.visible}><svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M22 2L11 13"/><path d="M22 2L15 22 11 13 2 9z"/></svg>Отправить файл</button></li>{/each}</ul>
                {/if}
            </div>
        {:else if activeTab === 'qr'}
            <div class="panel center-panel" in:fade={{ duration: 200 }}>
                <h2>Мобильное подключение</h2>
                <div class="toggle-container">
                    <button class:active={qrMode === 'receive'} on:click={() => { qrMode = 'receive'; qrDataUrl = ''; selectedFileName = ''; sendStatus = ''; generateQR(); }}><svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="7 10 12 15 17 10"/><line x1="12" y1="15" x2="12" y2="3"/></svg>Получить с телефона</button>
                    <button class:active={qrMode === 'send'} on:click={() => { qrMode = 'send'; qrDataUrl = ''; selectedFileName = ''; sendStatus = ''; }}><svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="17 8 12 3 7 8"/><line x1="12" y1="3" x2="12" y2="15"/></svg>Отправить на телефон</button>
                </div>
                {#if qrMode === 'receive'}
                    {#if qrError}<div class="error-state" in:fade><p>{qrError}</p></div>
                    {:else if qrDataUrl}
                        <div class="qr-wrapper" in:fade={{ duration: 300 }}><div class="qr-container"><img src={qrDataUrl} alt="QR Code" /><div class="qr-scan-line"></div></div></div>
                        <div class="info-grid"><div class="info-item"><span class="label">Код доступа</span><span class="value code">{qrCode}</span></div><details class="info-item ip-details"><summary class="label">Сетевой адрес</summary><span class="value">{localIp}:7431</span></details></div>
                    {:else}<div class="loading" in:fade><div class="spinner"></div><p>Генерация QR-кода...</p></div>{/if}
                {:else}
                    <div class="send-mode" class:drag-over={isDragging} on:dragover|preventDefault={() => isDragging = true} on:dragleave|preventDefault={() => isDragging = false} on:drop|preventDefault={handleDrop} in:fade={{ duration: 300 }}>
                        {#if selectedFileName}
                            <div class="file-selected" in:fly={{ y: 10, duration: 200 }}><div class="file-icon"><svg width="32" height="32" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><polyline points="14 2 14 8 20 8"/></svg></div><p class="file-name">{selectedFileName}</p><p class="file-hint">Отсканируйте QR-код телефоном для скачивания</p></div>
                            {#if qrDataUrl}<div class="qr-container" in:fade={{ duration: 300 }}><img src={qrDataUrl} alt="QR Code" /></div>{/if}
                            <button class="btn-secondary" on:click={() => { selectedFileName = ''; qrDataUrl = ''; sendStatus = ''; selectFileForSend(); }}>Выбрать другой файл</button>
                        {:else}
                            <div class="send-placeholder"><svg width="48" height="48" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="17 8 12 3 7 8"/><line x1="12" y1="3" x2="12" y2="15"/></svg><p class="send-hint">Выберите файл или перетащите его сюда</p></div>
                            {#if sendStatus && sendStatus.includes('Подготовка')}<div class="loading"><div class="spinner"></div><p>{sendStatus}</p></div>
                            {:else}<button class="btn-primary large" on:click={() => selectFileForSend()}><svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="17 8 12 3 7 8"/><line x1="12" y1="3" x2="12" y2="15"/></svg>Выбрать файл</button>{/if}
                        {/if}
                        {#if sendStatus && !sendStatus.includes('Подготовка')}<p class="status-message">{sendStatus}</p>{/if}
                    </div>
                {/if}
            </div>
        {:else if activeTab === 'settings'}
            <div class="panel" in:fade={{ duration: 200 }}>
                <h2>Настройки приложения</h2>
                <div class="form-group"><label>Тема оформления</label><select bind:value={theme} class="input-field"><option value="system">Системная</option><option value="light">Светлая</option><option value="dark">Темная</option></select></div>
                <div class="form-group"><label>Ручной IP-адрес</label><input type="text" bind:value={manualIp} placeholder="192.168.1.100" class="input-field" /><span class="hint">Оставьте пустым для автоопределения (исключает VPN/WSL)</span></div>
                <p class="auto-save-hint"><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M22 11.08V12a10 10 0 1 1-5.93-9.14"/><polyline points="22 4 12 14.01 9 11.01"/></svg>Настройки сохраняются автоматически</p>
            </div>
        {/if}
    </main>

    {#if pendingFile}<div class="notification" in:fly={{ y: 20, duration: 300 }}><div class="notif-header"><span class="notif-title">Получен файл</span><span class="notif-size">{(pendingFile.size / 1024 / 1024).toFixed(2)} MB</span></div><div class="notif-filename">{pendingFile.filename}</div><button class="btn-primary full-width" on:click={handleSave} disabled={saveStatus !== ''}>{saveStatus || 'Сохранить как...'}</button></div>{/if}
    {#if transferProgress.visible}<div class="transfer-progress" in:fly={{ y: 20, duration: 300 }}><div class="progress-header"><span>Отправка: {transferProgress.filename}</span><span>{transferProgress.percent}%</span></div><div class="progress-bar"><div class="progress-fill" style="width: {transferProgress.percent}%"></div></div></div>{/if}
    <div class="toast-container">{#each toasts as toast (toast.id)}<div class="toast toast-{toast.type}" in:fly={{ x: 100, duration: 300 }} out:fade={{ duration: 200 }}>{#if toast.type === 'success'}<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M22 11.08V12a10 10 0 1 1-5.93-9.14"/><polyline points="22 4 12 14.01 9 11.01"/></svg>{:else if toast.type === 'error'}<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><line x1="15" y1="9" x2="9" y2="15"/><line x1="9" y1="9" x2="15" y2="15"/></svg>{:else}<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><line x1="12" y1="16" x2="12" y2="12"/><line x1="12" y1="8" x2="12.01" y2="8"/></svg>{/if}<span>{toast.message}</span></div>{/each}</div>
</div>

<style>
    :global(:root) { --transition-speed: 0.3s; --font-sans: 'Segoe UI Variable', 'Segoe UI', system-ui, -apple-system, sans-serif; --font-mono: 'Cascadia Code', 'Fira Code', Consolas, monospace; }
    :global(.theme-light) { --bg-app: #f8fafc; --bg-panel: #ffffff; --bg-panel-hover: #f1f5f9; --text-main: #0f172a; --text-muted: #64748b; --border-color: #e2e8f0; --primary: #6366f1; --primary-hover: #4f46e5; --primary-glow: rgba(99, 102, 241, 0.4); --success: #10b981; --error: #ef4444; --shadow-sm: 0 1px 2px 0 rgb(0 0 0 / 0.05); --shadow-md: 0 4px 6px -1px rgb(0 0 0 / 0.1), 0 2px 4px -2px rgb(0 0 0 / 0.1); --shadow-lg: 0 10px 15px -3px rgb(0 0 0 / 0.1), 0 4px 6px -4px rgb(0 0 0 / 0.1); }
    :global(.theme-dark) { --bg-app: #0f172a; --bg-panel: #1e293b; --bg-panel-hover: #334155; --text-main: #f8fafc; --text-muted: #94a3b8; --border-color: #334155; --primary: #6366f1; --primary-hover: #4f46e5; --primary-glow: rgba(99, 102, 241, 0.4); --success: #10b981; --error: #ef4444; --shadow-md: 0 4px 6px -1px rgb(0 0 0 / 0.3), 0 2px 4px -2px rgb(0 0 0 / 0.3); --shadow-lg: 0 10px 15px -3px rgb(0 0 0 / 0.5), 0 4px 6px -4px rgb(0 0 0 / 0.5); }
    
    .app-layout { display: flex; height: 100vh; background: var(--bg-app); transition: background-color var(--transition-speed) ease; }
    .sidebar { width: 240px; background: var(--bg-panel); border-right: 1px solid var(--border-color); display: flex; flex-direction: column; padding: 1.5rem 0; transition: background-color var(--transition-speed) ease, border-color var(--transition-speed) ease; }
    .logo-section { padding: 0 1.5rem 1.5rem 1.5rem; border-bottom: 1px solid var(--border-color); margin-bottom: 1rem; display: flex; align-items: center; gap: 0.75rem; }
    .logo-icon { width: 36px; height: 36px; background: linear-gradient(135deg, var(--primary) 0%, #818cf8 100%); border-radius: 8px; display: flex; align-items: center; justify-content: center; color: white; box-shadow: var(--shadow-md); }
    .logo-section h1 { margin: 0; font-size: 1.25rem; font-weight: 700; color: var(--text-main); letter-spacing: -0.025em; }
    .nav-menu { display: flex; flex-direction: column; gap: 0.25rem; padding: 0 0.75rem; }
    .nav-menu button { display: flex; align-items: center; gap: 0.75rem; padding: 0.75rem 1rem; background: transparent; border: none; border-radius: 8px; color: var(--text-muted); font-size: 0.9rem; font-weight: 500; cursor: pointer; transition: all 0.2s cubic-bezier(0.4, 0, 0.2, 1); text-align: left; outline: none; }
    .nav-menu button:hover { background: var(--bg-panel-hover); color: var(--text-main); }
    .nav-menu button.active { background: var(--primary); color: white; box-shadow: var(--shadow-md); }
    .main-content { flex: 1; padding: 2rem; overflow-y: auto; background: var(--bg-app); transition: background-color var(--transition-speed) ease; }
    .panel { background: var(--bg-panel); border: 1px solid var(--border-color); border-radius: 16px; padding: 2rem; max-width: 700px; margin: 0 auto; box-shadow: var(--shadow-md); transition: background-color var(--transition-speed) ease, border-color var(--transition-speed) ease, box-shadow var(--transition-speed) ease; }
    .panel h2 { margin: 0 0 1.5rem 0; font-size: 1.5rem; font-weight: 700; color: var(--text-main); letter-spacing: -0.025em; }
    .center-panel { display: flex; flex-direction: column; align-items: center; text-align: center; }
    .toggle-container { display: flex; gap: 0.5rem; margin-bottom: 2rem; background: var(--bg-app); padding: 0.375rem; border-radius: 12px; border: 1px solid var(--border-color); }
    .toggle-container button { display: flex; align-items: center; justify-content: center; gap: 0.5rem; flex: 1; padding: 0.75rem 1rem; background: transparent; border: none; border-radius: 8px; color: var(--text-muted); font-size: 0.9rem; font-weight: 500; cursor: pointer; transition: all 0.2s ease; outline: none; }
    .toggle-container button.active { background: var(--primary); color: white; box-shadow: var(--shadow-md); }
    .qr-wrapper { margin-bottom: 2rem; }
    .qr-container { position: relative; display: inline-block; padding: 1rem; background: #ffffff; border-radius: 12px; border: 1px solid var(--border-color); box-shadow: var(--shadow-lg); overflow: hidden; transition: transform 0.3s cubic-bezier(0.34, 1.56, 0.64, 1); }
    .qr-container:hover { transform: scale(1.02); }
    .qr-container img { display: block; border-radius: 8px; }
    .qr-scan-line { position: absolute; left: 1rem; right: 1rem; height: 2px; background: linear-gradient(90deg, transparent, var(--primary), transparent); animation: scan 2s ease-in-out infinite; }
    @keyframes scan { 0%, 100% { top: 1rem; opacity: 0; } 10%, 90% { opacity: 1; } 50% { top: calc(100% - 1rem); } }
    .info-grid { display: grid; grid-template-columns: 1fr 1fr; gap: 1rem; width: 100%; max-width: 400px; }
    .info-item { display: flex; flex-direction: column; gap: 0.5rem; padding: 1rem; background: var(--bg-app); border-radius: 8px; border: 1px solid var(--border-color); }
    .ip-details summary { cursor: pointer; list-style: none; display: flex; align-items: center; gap: 0.5rem; }
    .ip-details summary::after { content: '▼'; font-size: 0.7rem; margin-left: auto; transition: transform 0.2s; }
    .ip-details[open] summary::after { transform: rotate(180deg); }
    .ip-details summary::-webkit-details-marker { display: none; }
    .label { font-size: 0.75rem; color: var(--text-muted); text-transform: uppercase; letter-spacing: 0.05em; font-weight: 600; }
    .value { font-size: 1.1rem; font-weight: 600; font-family: var(--font-mono); }
    .value.code { letter-spacing: 0.25rem; color: var(--primary); font-size: 1.5rem; }
    .empty-state, .error-state, .loading { text-align: center; padding: 3rem 1rem; color: var(--text-muted); }
    .empty-icon { margin-bottom: 1rem; opacity: 0.5; }
    .empty-title { font-size: 1.1rem; font-weight: 600; color: var(--text-main); margin: 0 0 0.5rem 0; }
    .empty-subtitle { font-size: 0.9rem; margin: 0 0 1.5rem 0; }
    .error-state { color: var(--error); }
    .troubleshoot { margin-top: 1.5rem; text-align: left; background: var(--bg-app); border: 1px solid var(--border-color); border-radius: 8px; padding: 1rem; }
    .troubleshoot summary { cursor: pointer; font-weight: 600; color: var(--text-main); }
    .troubleshoot ul { margin: 0.75rem 0 0 1.5rem; padding: 0; font-size: 0.875rem; color: var(--text-muted); }
    .troubleshoot li { margin-bottom: 0.5rem; }
    .send-mode { width: 100%; max-width: 400px; transition: all 0.2s; border: 2px dashed var(--border-color); border-radius: 16px; padding: 2rem; display: flex; flex-direction: column; align-items: center; }
    .send-mode.drag-over { border-color: var(--primary); background: rgba(99, 102, 241, 0.05); transform: scale(1.02); }
    .send-placeholder { display: flex; flex-direction: column; align-items: center; gap: 1rem; margin-bottom: 1.5rem; opacity: 0.6; }
    .send-hint { color: var(--text-muted); font-size: 1rem; margin: 0; }
    .file-selected { margin-bottom: 1.5rem; display: flex; flex-direction: column; align-items: center; gap: 0.5rem; }
    .file-icon { color: var(--primary); margin-bottom: 0.5rem; }
    .file-name { font-size: 1.1rem; font-weight: 600; color: var(--text-main); margin: 0; word-break: break-all; }
    .file-hint { color: var(--text-muted); font-size: 0.875rem; margin: 0; }
    .status-message { margin-top: 1rem; color: var(--primary); font-weight: 500; }
    .device-list { list-style: none; padding: 0; margin: 0; display: flex; flex-direction: column; gap: 0.75rem; }
    .device-item { display: flex; justify-content: space-between; align-items: center; padding: 1rem 1.25rem; background: var(--bg-app); border: 1px solid var(--border-color); border-radius: 8px; transition: all 0.2s ease; }
    .device-item:hover { border-color: var(--primary); box-shadow: var(--shadow-sm); }
    .device-info { display: flex; flex-direction: column; }
    .device-header { display: flex; align-items: center; gap: 0.5rem; margin-bottom: 0.25rem; }
    .device-name { font-weight: 600; }
    .device-ip { font-size: 0.85rem; color: var(--text-muted); font-family: var(--font-mono); }
    .status-dot { width: 8px; height: 8px; background: var(--success); border-radius: 50%; position: relative; }
    .status-dot::after { content: ''; position: absolute; top: 0; left: 0; width: 100%; height: 100%; border-radius: 50%; background: var(--success); animation: pulse 2s infinite; }
    @keyframes pulse { 0% { transform: scale(1); opacity: 0.8; } 100% { transform: scale(3); opacity: 0; } }
    .form-group { margin-bottom: 1.5rem; }
    .form-group label { display: block; margin-bottom: 0.5rem; font-weight: 500; color: var(--text-main); font-size: 0.9rem; }
    .input-field { width: 100%; padding: 0.75rem 1rem; background: var(--bg-app); border: 1px solid var(--border-color); border-radius: 8px; color: var(--text-main); font-size: 0.9rem; font-family: var(--font-sans); box-sizing: border-box; transition: all 0.2s ease; outline: none; }
    .input-field:focus { border-color: var(--primary); box-shadow: 0 0 0 3px var(--primary-glow); }
    .form-group .hint, .auto-save-hint { display: flex; align-items: center; gap: 0.5rem; margin-top: 0.5rem; font-size: 0.85rem; color: var(--text-muted); font-style: italic; }
    .btn-primary { display: inline-flex; align-items: center; justify-content: center; gap: 0.5rem; padding: 0.75rem 1.5rem; font-family: var(--font-sans); font-weight: 500; font-size: 0.9rem; color: #ffffff; background: linear-gradient(135deg, var(--primary) 0%, #818cf8 100%); border: none; border-radius: 8px; cursor: pointer; box-shadow: var(--shadow-sm); transition: all 0.2s cubic-bezier(0.4, 0, 0.2, 1); position: relative; overflow: hidden; outline: none; }
    .btn-primary:hover:not(:disabled) { transform: translateY(-1px); box-shadow: var(--shadow-md), 0 0 15px var(--primary-glow); filter: brightness(1.05); }
    .btn-primary:active:not(:disabled) { transform: translateY(1px) scale(0.98); }
    .btn-primary:disabled { opacity: 0.5; cursor: not-allowed; }
    .btn-primary.large { padding: 1rem 2rem; font-size: 1rem; }
    .btn-secondary { display: inline-flex; align-items: center; gap: 0.5rem; background: var(--bg-app); color: var(--text-main); border: 1px solid var(--border-color); padding: 0.75rem 1.5rem; border-radius: 8px; font-weight: 500; font-size: 0.9rem; cursor: pointer; transition: all 0.2s ease; outline: none; }
    .btn-secondary:hover { background: var(--bg-panel-hover); border-color: var(--primary); }
    .full-width { width: 100%; margin-top: 0.75rem; }
    .notification { position: fixed; bottom: 2rem; right: 2rem; width: 320px; background: var(--bg-panel); border: 1px solid var(--border-color); border-left: 4px solid var(--primary); border-radius: 12px; padding: 1.25rem; box-shadow: var(--shadow-lg); z-index: 100; }
    .notif-header { display: flex; justify-content: space-between; margin-bottom: 0.5rem; }
    .notif-title { font-weight: 600; }
    .notif-size { font-size: 0.85rem; color: var(--text-muted); }
    .notif-filename { font-size: 0.9rem; color: var(--text-muted); word-break: break-all; margin-bottom: 0.75rem; }
    .spinner { width: 32px; height: 32px; border: 3px solid var(--border-color); border-top-color: var(--primary); border-radius: 50%; animation: spin 0.8s linear infinite; margin: 0 auto 1rem; }
    @keyframes spin { to { transform: rotate(360deg); } }
    .transfer-progress { position: fixed; bottom: 2rem; right: 2rem; width: 320px; background: var(--bg-panel); border: 1px solid var(--border-color); border-left: 4px solid var(--primary); border-radius: 12px; padding: 1.25rem; box-shadow: var(--shadow-lg); z-index: 100; }
    .progress-header { display: flex; justify-content: space-between; margin-bottom: 0.75rem; font-size: 0.9rem; font-weight: 500; }
    .progress-bar { width: 100%; height: 8px; background: var(--bg-app); border-radius: 8px; overflow: hidden; }
    .progress-fill { height: 100%; background: linear-gradient(90deg, var(--primary), var(--primary-hover)); transition: width 0.3s ease; border-radius: 8px; }
    .toast-container { position: fixed; top: 2rem; right: 2rem; display: flex; flex-direction: column; gap: 0.75rem; z-index: 1000; pointer-events: none; }
    .toast { display: flex; align-items: center; gap: 0.75rem; padding: 0.875rem 1.25rem; background: var(--bg-panel); border: 1px solid var(--border-color); border-radius: 8px; box-shadow: var(--shadow-lg); font-size: 0.9rem; font-weight: 500; pointer-events: auto; min-width: 250px; }
    .toast-success { border-left: 4px solid var(--success); color: var(--success); }
    .toast-error { border-left: 4px solid var(--error); color: var(--error); }
    .toast-info { border-left: 4px solid var(--primary); color: var(--primary); }
</style>
