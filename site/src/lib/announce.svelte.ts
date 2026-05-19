// Shared polite live region state. One `aria-live` node lives in the
// layout; any component calls `announce()` to speak through it. Message
// is cleared then re-set so repeated identical announcements
// (e.g. copying twice) are still voiced by screen readers.

export const live = $state<{ message: string }>({ message: "" });

let timer: ReturnType<typeof setTimeout> | undefined;

export function announce(message: string): void {
	live.message = "";
	clearTimeout(timer);
	timer = setTimeout(() => {
		live.message = message;
	}, 50);
}
