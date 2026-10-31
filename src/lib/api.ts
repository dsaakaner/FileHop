export async function getStatus() {
    try {
        const response = await fetch('http://localhost:7431/api/status');
        return await response.json();
    } catch (error) {
        console.error("Failed to fetch status:", error);
        return { status: 'offline', code: '---', clients_count: 0 };
    }
}
