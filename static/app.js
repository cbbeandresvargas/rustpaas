/**
 * RustPaaS — app.js
 * Handles: project actions (restart/stop), log polling, auto-scroll
 */

// ─── Project actions ───────────────────────────────────────────────────────

async function restartProject(projectId) {
    if (!confirm('¿Reiniciar este proyecto?')) return;
    try {
        const res = await fetch(`/api/projects/${projectId}/restart`, { method: 'POST' });
        const data = await res.json();
        if (res.ok) {
            showToast('✅ Proyecto reiniciado', 'success');
            updateRowStatus(projectId, 'running');
        } else {
            showToast(`❌ Error: ${data.error}`, 'error');
        }
    } catch (e) {
        showToast(`❌ Error de red: ${e.message}`, 'error');
    }
}

async function stopProject(projectId) {
    if (!confirm('¿Detener este proyecto?')) return;
    try {
        const res = await fetch(`/api/projects/${projectId}/stop`, { method: 'POST' });
        const data = await res.json();
        if (res.ok) {
            showToast('⏹ Proyecto detenido', 'success');
            updateRowStatus(projectId, 'stopped');
        } else {
            showToast(`❌ Error: ${data.error}`, 'error');
        }
    } catch (e) {
        showToast(`❌ Error de red: ${e.message}`, 'error');
    }
}

/** Update the status badge in the project table row (index page) */
function updateRowStatus(projectId, status) {
    const row = document.getElementById(`project-row-${projectId}`);
    if (!row) return;

    const badge = row.querySelector('.badge');
    if (!badge) return;

    const labels = {
        running:   'Running',
        stopped:   'Stopped',
        deploying: 'Deploying',
        suspended: 'Suspended',
        error:     'Error',
    };

    badge.className = `badge badge-${status}`;
    badge.textContent = labels[status] ?? status;
}

// ─── Toast notifications ───────────────────────────────────────────────────

function showToast(message, type = 'success') {
    const existing = document.querySelector('.toast');
    if (existing) existing.remove();

    const toast = document.createElement('div');
    toast.className = `toast toast-${type}`;
    toast.textContent = message;
    toast.style.cssText = `
        position: fixed;
        bottom: 1.5rem;
        right: 1.5rem;
        background: ${type === 'success' ? '#1a2e1a' : '#2e1a1a'};
        border: 1px solid ${type === 'success' ? '#22c55e' : '#ef4444'};
        color: ${type === 'success' ? '#22c55e' : '#ef4444'};
        padding: 0.75rem 1.25rem;
        border-radius: 8px;
        font-size: 0.875rem;
        font-weight: 500;
        z-index: 9999;
        animation: fadeIn 0.2s ease;
        box-shadow: 0 8px 32px rgba(0,0,0,0.4);
    `;

    const style = document.createElement('style');
    style.textContent = `@keyframes fadeIn { from { opacity: 0; transform: translateY(8px); } to { opacity: 1; transform: none; } }`;
    document.head.appendChild(style);

    document.body.appendChild(toast);
    setTimeout(() => toast.remove(), 3500);
}

// ─── Live log polling (project detail page) ────────────────────────────────

let autoScroll = true;
let logPollInterval = null;

function toggleAutoScroll() {
    autoScroll = !autoScroll;
    const btn = document.getElementById('btn-autoscroll');
    if (btn) btn.textContent = `Auto-scroll: ${autoScroll ? 'ON' : 'OFF'}`;
}

function startLogPolling(projectId) {
    const viewer = document.getElementById('log-viewer');
    const statusDot = document.getElementById('log-status-dot');
    const statusText = document.getElementById('log-status-text');

    if (!viewer) return;

    async function fetchLogs() {
        try {
            const res = await fetch(`/projects/${projectId}/logs`);
            if (!res.ok) throw new Error(`HTTP ${res.status}`);
            const text = await res.text();
            viewer.textContent = text;

            if (autoScroll) {
                viewer.scrollTop = viewer.scrollHeight;
            }

            if (statusDot) statusDot.style.background = '#22c55e';
            if (statusText) statusText.textContent = 'En vivo';
        } catch (e) {
            if (statusDot) statusDot.style.background = '#ef4444';
            if (statusText) statusText.textContent = 'Sin conexión';
        }
    }

    // Initial load, then poll every 2 seconds
    fetchLogs();
    logPollInterval = setInterval(fetchLogs, 2000);
}

// ─── Init ──────────────────────────────────────────────────────────────────

document.addEventListener('DOMContentLoaded', () => {
    // Start log polling if we're on the project detail page
    const projectId = window.MYPAAS_PROJECT_ID;
    if (projectId) {
        startLogPolling(projectId);
        loadEnv(projectId);
    }
});

// ─── Environment Variables (.env) ──────────────────────────────────────────

async function loadEnv(projectId) {
    const editor = document.getElementById('env-editor');
    if (!editor) return;

    try {
        const res = await fetch(`/api/projects/${projectId}/env`);
        if (res.ok) {
            const text = await res.text();
            editor.value = text;
        }
    } catch (e) {
        console.error("Failed to load .env:", e);
    }
}

async function saveEnv(projectId) {
    const editor = document.getElementById('env-editor');
    if (!editor) return;
    const btn = document.getElementById('btn-save-env');
    if (btn) btn.disabled = true;

    try {
        const res = await fetch(`/api/projects/${projectId}/env`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ env_content: editor.value }),
        });
        const data = await res.json();
        
        if (res.ok) {
            showToast('✅ Variables guardadas y proyecto reiniciado', 'success');
        } else {
            showToast(`❌ Error al guardar: ${data.error}`, 'error');
        }
    } catch (e) {
        showToast(`❌ Error de red: ${e.message}`, 'error');
    } finally {
        if (btn) btn.disabled = false;
    }
}

// Cleanup on page unload
window.addEventListener('beforeunload', () => {
    if (logPollInterval) clearInterval(logPollInterval);
});

// ─── New Project Modal ─────────────────────────────────────────────────────

function openNewProjectModal() {
    const modal = document.getElementById('new-project-modal');
    if (modal) {
        modal.style.display = 'flex';
        document.getElementById('new-project-name').focus();
    }
}

function closeNewProjectModal() {
    const modal = document.getElementById('new-project-modal');
    if (modal) {
        modal.style.display = 'none';
        document.getElementById('new-project-form').reset();
    }
}

async function submitNewProject(e) {
    e.preventDefault();
    const btn = document.getElementById('btn-submit-project');
    const originalText = btn.textContent;
    btn.textContent = 'Subiendo...';
    btn.disabled = true;

    const form = document.getElementById('new-project-form');
    const formData = new FormData(form);

    try {
        const res = await fetch('/api/deploy', {
            method: 'POST',
            body: formData,
        });
        const data = await res.json();
        
        if (res.ok) {
            showToast('✅ Proyecto subido y desplegado con éxito', 'success');
            closeNewProjectModal();
            setTimeout(() => window.location.reload(), 1000);
        } else {
            showToast(`❌ Error: ${data.error}`, 'error');
        }
    } catch (err) {
        showToast(`❌ Error de red: ${err.message}`, 'error');
    } finally {
        btn.textContent = originalText;
        btn.disabled = false;
    }
}

// Close modal when clicking outside
window.addEventListener('click', (e) => {
    const modal = document.getElementById('new-project-modal');
    if (e.target === modal) {
        closeNewProjectModal();
    }
});

// ─── Copy API Key Command ──────────────────────────────────────────────────

function copyCurlCommand() {
    const cmdElement = document.getElementById('curl-cmd');
    if (!cmdElement) return;
    
    navigator.clipboard.writeText(cmdElement.textContent)
        .then(() => showToast('✅ Comando copiado al portapapeles', 'success'))
        .catch(err => showToast(`❌ Error al copiar: ${err}`, 'error'));
}
