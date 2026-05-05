const live = document.getElementById("copy-status");

for (const btn of document.querySelectorAll<HTMLButtonElement>(".copy")) {
	btn.addEventListener("click", async () => {
		const cmd = btn.dataset.cmd;
		if (!cmd) return;
		let ok = false;
		try {
			await navigator.clipboard.writeText(cmd);
			ok = true;
		} catch {}
		if (live) {
			const msg = ok ? "Copied to clipboard" : "Copy failed";
			live.textContent = "";
			setTimeout(() => {
				live.textContent = msg;
			}, 50);
		}
		if (!ok) return;
		btn.classList.add("copied");
		setTimeout(() => btn.classList.remove("copied"), 1400);
	});
}
