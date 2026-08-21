(function footballReference() {
    "use strict";

    const elements = {
        homeName: document.querySelector("#home-name"),
        homeScore: document.querySelector("#home-score"),
        awayName: document.querySelector("#away-name"),
        awayScore: document.querySelector("#away-score"),
        competition: document.querySelector("#competition"),
        clock: document.querySelector("#clock"),
        period: document.querySelector("#period"),
        addedTime: document.querySelector("#added-time"),
        addedTimeWrap: document.querySelector("#added-time-wrap"),
        homeYellow: document.querySelector("#home-yellow"),
        awayYellow: document.querySelector("#away-yellow"),
        homeYellowWrap: document.querySelector("#home-yellow-wrap"),
        awayYellowWrap: document.querySelector("#away-yellow-wrap"),
        homeRed: document.querySelector("#home-red"),
        awayRed: document.querySelector("#away-red"),
        homeRedWrap: document.querySelector("#home-red-wrap"),
        awayRedWrap: document.querySelector("#away-red-wrap"),
        board: document.querySelector(".scoreboard")
    };

    // period -> broadcast label. Falls back to "Half N" for anything past
    // extra time / penalties (e.g. a tournament with unusual rules).
    const PERIOD_LABELS = {
        1: "1st Half",
        2: "2nd Half",
        3: "Extra Time 1",
        4: "Extra Time 2",
        5: "Penalties"
    };

    const fallbackState = {
        version: 1,
        game: {
            competition: "Friendly",
            clock: "63:18",
            period: 2,
            running: false,
            status: "live"
        },
        home: {
            name: "Schapen Sharks FC",
            shortName: "SHARKS FC",
            score: 2,
            color: "#0057a8",
            secondaryColor: "#ffffff",
            logo: null
        },
        away: {
            name: "Braunschweig United FC",
            shortName: "UNITED FC",
            score: 1,
            color: "#cf2027",
            secondaryColor: "#ffffff",
            logo: null
        },
        sport: {
            periodCount: 2,
            periodDurationMinutes: 45,
            addedTime: 0,
            homeYellowCards: 0,
            awayYellowCards: 0,
            homeRedCards: 0,
            awayRedCards: 0
        },
        custom: {}
    };
    const query = new URLSearchParams(location.search);
    const editorMode = query.get("debug") === "1";
    const editorToken = query.get("recoEditorToken");
    let debugState = structuredClone(fallbackState);
    let timer = null;

    function text(element, value, fallback) {
        element.textContent = value ?? fallback;
    }

    function ensureStateShape(state) {
        state.game ??= {};
        state.home ??= {};
        state.away ??= {};
        state.sport ??= {};
        state.custom ??= {};
        state.sport.periodCount ??= 2;
        state.sport.periodDurationMinutes ??= 45;
        state.sport.addedTime ??= 0;
        state.sport.homeYellowCards ??= 0;
        state.sport.awayYellowCards ??= 0;
        state.sport.homeRedCards ??= 0;
        state.sport.awayRedCards ??= 0;
        state.game.period ??= 1;
        return state;
    }

    function setInput(id, value) {
        const input = document.querySelector(`#${id}`);
        if (input && document.activeElement !== input) input.value = value ?? "";
    }

    function syncEditorFields() {
        if (!editorMode) return;
        setInput("competition-input", debugState.game.competition);
        setInput("home-name-input", debugState.home.shortName || debugState.home.name);
        setInput("away-name-input", debugState.away.shortName || debugState.away.name);
        setInput("period-count-input", debugState.sport.periodCount);
        setInput("period-duration-input", debugState.sport.periodDurationMinutes);
        setInput("period-input", debugState.game.period);
        setInput("added-time-input", debugState.sport.addedTime);
        const clockButton = document.querySelector("#toggle-clock");
        if (clockButton) clockButton.textContent = timer ? "Stop clock" : "Start clock";
    }

    function cardBadge(wrap, valueEl, count) {
        const shown = Number(count) > 0;
        wrap.hidden = !shown;
        if (shown) valueEl.textContent = count;
    }

    Reco.onUpdate((state) => {
        debugState = ensureStateShape(structuredClone(state));
        text(elements.homeName, state.home?.shortName || state.home?.name, "HOME");
        text(elements.homeScore, state.home?.score, 0);
        text(elements.awayName, state.away?.shortName || state.away?.name, "AWAY");
        text(elements.awayScore, state.away?.score, 0);
        text(elements.competition, state.game?.competition, "");
        elements.competition.hidden = !state.game?.competition;
        text(elements.clock, state.game?.clock, "00:00");
        const period = state.game?.period ?? 1;
        text(elements.period, PERIOD_LABELS[period] || `Half ${period}`, "1st Half");
        const addedTime = Number(state.sport?.addedTime) || 0;
        elements.addedTimeWrap.hidden = addedTime <= 0;
        text(elements.addedTime, addedTime, "0");
        cardBadge(elements.homeYellowWrap, elements.homeYellow, state.sport?.homeYellowCards);
        cardBadge(elements.awayYellowWrap, elements.awayYellow, state.sport?.awayYellowCards);
        cardBadge(elements.homeRedWrap, elements.homeRed, state.sport?.homeRedCards);
        cardBadge(elements.awayRedWrap, elements.awayRed, state.sport?.awayRedCards);
        elements.board.style.setProperty("--home-color", state.home?.color || "#0057a8");
        elements.board.style.setProperty("--home-secondary", state.home?.secondaryColor || "#ffffff");
        elements.board.style.setProperty("--away-color", state.away?.color || "#cf2027");
        elements.board.style.setProperty("--away-secondary", state.away?.secondaryColor || "#ffffff");
        syncEditorFields();
    });

    function editorHeaders() {
        return {
            "Content-Type": "application/json",
            "X-Reco-Editor-Token": editorToken || ""
        };
    }

    async function publishRemote() {
        if (!editorToken) return;
        const status = document.querySelector("#editor-status");
        try {
            const response = await fetch("/__reco/editor-state", {
                method: "PUT",
                headers: editorHeaders(),
                body: JSON.stringify(debugState)
            });
            if (!response.ok) throw new Error(`HTTP ${response.status}`);
            if (status) {
                status.textContent = "Live";
                status.classList.remove("error");
            }
        } catch (error) {
            if (status) {
                status.textContent = "Not connected";
                status.classList.add("error");
            }
            console.error("Cannot publish scoreboard editor state", error);
        }
    }

    function publish() {
        RecoScoreboard.update(debugState);
        void publishRemote();
    }

    // Unlike basketball's countdown, football match time counts up and
    // carries across halves (e.g. second-half stoppage time reads
    // "90+3", not a fresh countdown) - "Next half" only advances the
    // period and clears added time, it never touches the clock.
    function tickClock() {
        const [minutes, seconds] = String(debugState.game.clock || "00:00").split(":").map(Number);
        const total = (minutes || 0) * 60 + (seconds || 0) + 1;
        debugState.game.clock = `${String(Math.floor(total / 60)).padStart(2, "0")}:${String(total % 60).padStart(2, "0")}`;
        publish();
    }

    function numberValue(id, minimum, maximum) {
        const value = Number(document.querySelector(`#${id}`).value);
        return Math.min(maximum, Math.max(minimum, Number.isFinite(value) ? value : minimum));
    }

    function bindEditorControls() {
        const controls = document.querySelector("#debug-controls");
        controls.hidden = false;
        controls.addEventListener("click", (event) => {
            const button = event.target.closest("button[data-score]");
            if (!button) return;
            const team = button.dataset.score;
            debugState[team].score = Number(debugState[team].score || 0) + Number(button.dataset.points);
            publish();
        });

        const textBindings = [
            ["competition-input", (value) => { debugState.game.competition = value.trim(); }],
            ["home-name-input", (value) => { debugState.home.name = value.trim(); debugState.home.shortName = value.trim(); }],
            ["away-name-input", (value) => { debugState.away.name = value.trim(); debugState.away.shortName = value.trim(); }]
        ];
        for (const [id, apply] of textBindings) {
            document.querySelector(`#${id}`).addEventListener("change", (event) => {
                apply(event.target.value);
                publish();
            });
        }

        document.querySelector("#period-count-input").addEventListener("change", () => {
            debugState.sport.periodCount = numberValue("period-count-input", 1, 5);
            debugState.game.period = Math.min(debugState.game.period, debugState.sport.periodCount);
            publish();
        });
        document.querySelector("#period-duration-input").addEventListener("change", () => {
            debugState.sport.periodDurationMinutes = numberValue("period-duration-input", 1, 60);
            publish();
        });
        document.querySelector("#period-input").addEventListener("change", () => {
            debugState.game.period = numberValue("period-input", 1, debugState.sport.periodCount);
            publish();
        });
        document.querySelector("#added-time-input").addEventListener("change", () => {
            debugState.sport.addedTime = numberValue("added-time-input", 0, 15);
            publish();
        });
        document.querySelector("#home-yellow-btn").addEventListener("click", () => {
            debugState.sport.homeYellowCards = Number(debugState.sport.homeYellowCards || 0) + 1;
            publish();
        });
        document.querySelector("#away-yellow-btn").addEventListener("click", () => {
            debugState.sport.awayYellowCards = Number(debugState.sport.awayYellowCards || 0) + 1;
            publish();
        });
        document.querySelector("#home-red-btn").addEventListener("click", () => {
            debugState.sport.homeRedCards = Number(debugState.sport.homeRedCards || 0) + 1;
            publish();
        });
        document.querySelector("#away-red-btn").addEventListener("click", () => {
            debugState.sport.awayRedCards = Number(debugState.sport.awayRedCards || 0) + 1;
            publish();
        });
        document.querySelector("#toggle-clock").addEventListener("click", () => {
            if (timer) {
                clearInterval(timer);
                timer = null;
                debugState.game.running = false;
            } else {
                debugState.game.running = true;
                timer = setInterval(tickClock, 1000);
            }
            publish();
        });
        document.querySelector("#reset-clock").addEventListener("click", () => {
            debugState.game.clock = "00:00";
            publish();
        });
        document.querySelector("#next-period").addEventListener("click", () => {
            debugState.game.period = Math.min(
                debugState.sport.periodCount,
                Number(debugState.game.period || 1) + 1
            );
            debugState.sport.addedTime = 0;
            publish();
        });
    }

    async function loadPublishedState() {
        if (!editorToken) return null;
        try {
            const response = await fetch("/__reco/editor-state", { headers: editorHeaders() });
            if (!response.ok) return null;
            return await response.json();
        } catch (_) {
            return null;
        }
    }

    async function initialize() {
        if (editorMode) {
            bindEditorControls();
            const published = await loadPublishedState();
            if (published && typeof published === "object") debugState = ensureStateShape(published);
        }
        RecoScoreboard.update(debugState);
        if (editorMode) void publishRemote();
        Reco.ready();
    }

    void initialize();
})();
