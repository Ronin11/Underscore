#!/bin/sh
# Set up the map worker on a machine with an NVIDIA GPU, Miniconda, and
# Ollama: a conda environment, the package, and a systemd user service.
# Run from a checkout at ~/underscore.
set -eu
CONDA="${CONDA:-$HOME/miniconda3/bin/conda}"
ENV=underscore-worker
here="$(cd "$(dirname "$0")/../.." && pwd)"

if ! "$CONDA" env list | grep -q "^$ENV "; then
    "$CONDA" create -y -n "$ENV" python=3.11
fi
PY="$HOME/miniconda3/envs/$ENV/bin/python"
# Torch's CUDA wheels carry their own CUDA and cuDNN; only the driver is
# needed. Any CUDA 12 build runs on a CUDA 12 driver (535 and up).
"$PY" -m pip install --upgrade pip
"$PY" -m pip install torch --index-url https://download.pytorch.org/whl/cu126
"$PY" -m pip install -e "$here/worker"

mkdir -p "$HOME/.config/underscore" "$HOME/.config/systemd/user"
if [ ! -f "$HOME/.config/underscore/worker.env" ]; then
    cp "$here/deploy/worker/worker.env.example" "$HOME/.config/underscore/worker.env"
    chmod 600 "$HOME/.config/underscore/worker.env"
    echo "Fill in $HOME/.config/underscore/worker.env, then run: systemctl --user enable --now underscore-worker"
fi
cp "$here/deploy/worker/underscore-worker.service" "$HOME/.config/systemd/user/"
systemctl --user daemon-reload
echo "To keep it running after you log out: sudo loginctl enable-linger $USER"
