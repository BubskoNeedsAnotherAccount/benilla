# Gold medal replays

Each course in `../../time_trials.json` names its gold medal replay with `goldMedalReplay`, a path
relative to `assets/`. Beat that replay's time to win the course's gold medal; while racing it
drives beside you as a translucent ghost (H shows or hides it).

To set one: finish the course, press **Save replay** on the finish panel, and put the saved
`.lambo-replay` file here under the name the course's `goldMedalReplay` gives. A course whose file
is missing races without a ghost and has no gold medal yet.

The browser build (`web/build.mjs`) copies every one that exists into the page.
