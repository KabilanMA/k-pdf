#!/usr/bin/env python3
"""Takes the website's screenshots by replaying input in the viewer.

    site/screenshots/make.sh            # makes the sample files, then runs this
    python3 site/screenshots/scenes.py [dark|light|both] [scene ...]

Each scene opens sample files in a viewer built with the `script` feature
(target/script/release/k-pdf), plays its steps (see viewer/src/script.rs)
and saves the window as site/static/images/<scene>-<theme>.webp. The light
versions switch the theme through the ⋯ menu first; views without that menu
(the start screen, the password prompt) are taken in the dark theme only,
and the website uses those for both themes.

The viewer runs with HOME in the work folder, so it doesn't read or change
your remembered reading positions, and with USER=alex, the name shown on
comments. Coordinates are for the 1000×800 window; take a `shot` to find new
ones after the layout changes.
"""
import os
import shutil
import subprocess
import sys
import textwrap

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
WORK = os.path.join(ROOT, "target", "site-shots")
DOCS = os.path.join(WORK, "docs")
OUT = os.path.join(ROOT, "site", "static", "images")
VIEWER = os.path.join(ROOT, "target", "script", "release", "k-pdf")

START = "move 600 400\nwait 30\npause 300\n"
# Switches to the light theme through the ⋯ menu (document views).
LIGHT_DOC = "click 875 23\nwait 10\npause 300\nclick 729 538\nwait 5\nclick 300 787\nwait 10\npause 300\n"

SCENES = {
    # name: (files, steps, light prefix or None when the view has no theme menu)
    "start": ([], "wait 20\npause 300\nshot {shot}\n", None),
    "password": (["locked.pdf"], START + "text secret\nwait 5\nshot {shot}\n", None),
    "read": (["streams.pdf"], START + "shot {shot}\n", LIGHT_DOC),
    "sections": (["streams.pdf"], START + "click 103 69\nwait 10\npause 300\nmove 600 400\nwait 5\nshot {shot}\n", LIGHT_DOC),
    "search": (["streams.pdf"], START + "key ctrl+f\nwait 5\ntext stream\nwait 10\npause 1500\nshot {shot}\n", LIGHT_DOC),
    "highlight": (["streams.pdf"], START + """
press 338 397
wait 3
move 500 410
wait 3
move 640 415
wait 3
release 640 415
wait 10
pause 400
shot {shot}
""", LIGHT_DOC),
    "comment": (["streams.pdf"], START + """
press 338 397
wait 3
move 500 410
wait 3
move 640 415
wait 3
release 640 415
wait 10
pause 300
click 683 360
wait 10
pause 300
text Check this in the morning.
wait 10
pause 300
shot {shot}
""", LIGHT_DOC),
    "edit": (["streams.pdf"], START + """
key ctrl+e
wait 5
click 600 440
wait 10
key end
wait 3
text  Bring a map.
wait 10
pause 300
shot {shot}
""", LIGHT_DOC),
    "form-fill": (["signup.pdf"], START + """
click 606 255
wait 5
text Alex Morgan
wait 5
click 616 293
wait 5
text alex@example.com
wait 5
click 376 452
wait 5
click 531 452
wait 5
click 562 675
wait 10
pause 300
shot {shot}
""", "LIGHT_FORM"),
    "form-create": (["signup.pdf"], START + "click 315 23\nwait 10\npause 300\nclick 606 255\nwait 10\npause 300\nshot {shot}\n", "LIGHT_FORM"),
    "sign": (["streams.pdf"], START + "click 380 23\nwait 10\npause 300\nclick 213 225\nwait 5\ntext Alex Morgan\nwait 10\npause 800\nshot {shot}\n", LIGHT_DOC),
    "pages": (["streams.pdf"], START + "move 85 430\nwait 10\npause 400\nshot {shot}\n", LIGHT_DOC),
    "crop": (["streams.pdf"], START + """
click 218 23
wait 10
press 300 180
wait 3
move 500 300
wait 3
move 880 470
wait 3
release 880 470
wait 10
pause 300
shot {shot}
""", LIGHT_DOC),
    "rotate": (["streams.pdf"], START + "click 251 23\nwait 10\npause 300\nclick 743 77\nwait 10\npause 800\nshot {shot}\n", LIGHT_DOC),
    "redact": (["streams.pdf"], START + """
click 283 23
wait 10
press 338 397
wait 3
move 500 410
wait 3
move 640 415
wait 3
release 640 415
wait 10
pause 300
click 778 75
wait 10
pause 800
shot {shot}
""", LIGHT_DOC),
    "present": (["streams.pdf"], START + "key f5\nwait 10\npause 1500\nshot {shot}\n", LIGHT_DOC),
    "print": (["streams.pdf"], START + "key ctrl+p\nwait 10\npause 1500\nshot {shot}\n", LIGHT_DOC),
    "compress": (["streams.pdf"], START + "click 875 23\nwait 10\npause 300\nclick 693 390\nwait 10\npause 1500\nshot {shot}\n", LIGHT_DOC),
    "dark-pages": (["streams.pdf"], START + "click 496 23\nwait 10\npause 600\nshot {shot}\n", LIGHT_DOC),
    "photo": (["photo.jpg"], START + "pause 1500\nshot {shot}\n", "LIGHT_IMAGES"),
    "scan": (["photo.jpg"], START + "pause 2000\nclick 554 78\nwait 10\npause 4000\nshot {shot}\n", "LIGHT_IMAGES"),
    "cleanup": (["notes.pdf"], START + "click 542 23\nwait 10\npause 2000\nclick 690 78\nwait 10\npause 4000\nshot {shot}\n", LIGHT_DOC),
}

LIGHT_FORM = LIGHT_DOC.replace("729 538", "729 572")
LIGHT_IMAGES = "click 875 23\nwait 10\npause 300\nclick 760 196\nwait 5\nclick 300 787\nwait 10\npause 300\n"


def run(name, theme):
    files, steps, light = SCENES[name]
    if theme == "light":
        if light is None:
            return
        prefix = {"LIGHT_IMAGES": LIGHT_IMAGES, "LIGHT_FORM": LIGHT_FORM}.get(light, light)
        steps = START + prefix + steps
    missing = [f for f in files if not os.path.exists(os.path.join(DOCS, f))]
    if missing:
        print(f"skipped {name}: missing {', '.join(missing)}")
        return
    ppm = os.path.join(WORK, f"{name}-{theme}.ppm")
    steps = textwrap.dedent(steps).replace("{shot}", ppm)
    path = os.path.join(WORK, f"{name}-{theme}.txt")
    with open(path, "w") as f:
        f.write(steps)
    home = os.path.join(WORK, "home")
    shutil.rmtree(os.path.join(home, ".local", "state"), ignore_errors=True)
    env = dict(os.environ, HOME=home, USER="alex")
    env.pop("XDG_STATE_HOME", None)
    env.pop("XDG_DATA_HOME", None)
    args = [VIEWER] + [os.path.join(DOCS, f) for f in files] + ["--script", path]
    subprocess.run(args, env=env, timeout=180, cwd=DOCS)
    if not os.path.exists(ppm):
        print(f"no screenshot for {name} ({theme})")
        return
    subprocess.run(["magick", ppm, "-quality", "88", os.path.join(OUT, f"{name}-{theme}.webp")], check=True)
    os.remove(ppm)
    print(f"{name}-{theme}.webp")


def main():
    if not os.path.exists(VIEWER):
        sys.exit("Build the viewer first: cargo build --release -p k-pdf-viewer --features script --target-dir target/script")
    os.makedirs(OUT, exist_ok=True)
    # The OCR models, so search can read the text in images.
    models = os.path.expanduser("~/.local/share/k-pdf/models")
    link = os.path.join(WORK, "home", ".local", "share", "k-pdf", "models")
    if os.path.isdir(models) and not os.path.exists(link):
        os.makedirs(os.path.dirname(link), exist_ok=True)
        os.symlink(models, link)
    args = sys.argv[1:]
    which = args.pop(0) if args and args[0] in ("dark", "light", "both") else "both"
    themes = ["dark", "light"] if which == "both" else [which]
    for name in args or list(SCENES):
        if name not in SCENES:
            sys.exit(f"Unknown scene {name}; the scenes are: {', '.join(SCENES)}")
        for theme in themes:
            run(name, theme)


if __name__ == "__main__":
    main()
