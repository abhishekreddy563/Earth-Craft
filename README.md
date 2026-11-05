# Earth Pulse with EarthCraft

Earth Pulse extends the EarthCraft desktop globe with a Python analysis API, model selection for NVIDIA Nemotron and Ollama, geographic tools, and analysis overlays.

**Project origin:** this application combines the EarthCraft globe with a new backend developed for Earth Pulse. The other Earth projects compared during planning informed the design; their repositories were not merged into this codebase.

The backend runs on `127.0.0.1:8765`, processes the selected geometry, calls the selected AI provider, and returns measurements and overlays.

## Start on Windows

On a fresh checkout, open PowerShell in this folder, create the Python environment, install the backend dependencies, build the desktop app, then launch Earth Pulse.

```powershell
py -3.12 -m venv .venv
.\.venv\Scripts\python.exe -m pip install -r requirements.txt
.\Build-EarthPulse.ps1
.\Start-EarthPulse.ps1
```

The launcher starts the backend and the desktop with a temporary session token. Closing the desktop stops that backend. For API-only work, set a strong EARTH_PULSE_TOKEN in the current shell and run `Start-EarthPulse.ps1 -BackendOnly`. All API requests require an Authorization header containing Bearer followed by the token. The launcher leaves an already occupied port alone.

The `.venv` and `.build` directories are local environments and are not part of the repository. A fresh checkout needs Python 3.12 and Rust 1.95 or newer with the Windows native build prerequisites.

On Windows, `Build-EarthPulse.ps1` uses a matching MSVCRT MinGW toolchain at the drive root in `EarthPulseBuildTools` (for this project: `E:\EarthPulseBuildTools`). Keep that compiler path free of spaces for GNU linker compatibility.

## First measurement

1. Open **Earth Pulse** in the globe toolbar. Choose **Line** and use the globe context menu to add two points to the selection.
2. Choose **Distance** and click **Run tool**. The result shows the WGS84 distance in kilometres and the calculation method. Use **Polygon** and **Area** to measure a region with at least three points.
3. Choose **Ollama**, refresh the model list, select a model, and click **Verify capabilities**. Probes execute on that endpoint. Ask a model with verified tool support to measure the selected line. A verified text model can also explain a completed measurement using the panel's result option.
4. With Earth Engine configured, draw a polygon, enter inclusive dates, then select the vegetation tool. The backend returns a masked Sentinel-2 NDVI summary and a raster overlay. For change, enter a second period after the first. Elevation uses the SRTM dataset. Coverage depends on the region and acquisition dates.

## Backend configuration

Set these variables in the launcher environment. Keep service keys out of source control and desktop settings.

| Variable | Purpose |
|---|---|
| NVIDIA_API_KEY / NVIDIA_BASE_URL | Access Nemotron. The default URL is https://integrate.api.nvidia.com/v1. |
| OLLAMA_BASE_URL / OLLAMA_API_KEY | Native Ollama endpoint and optional credential. Default http://127.0.0.1:11434. |
| OLLAMA_CPU_ONLY | The launcher defaults to 1 for this computer. Set 0 to use Ollama's default hardware selection. |
| OLLAMA_LOAD_TIMEOUT_SECONDS | Cold model loading during the Ollama text capability check allows 600 seconds by default. Set an integer from 30 to 900. Tool and image checks and ordinary chat allow 180 seconds per request. |
| OLLAMA_ALLOW_CLOUD | Set 1 only for intended remote or cloud Ollama inference. |
| EE_PROJECT | Google Cloud project registered for Earth Engine. |
| GOOGLE_APPLICATION_CREDENTIALS | Optional path to credentials for the registered project's authorized principal, or configure Application Default Credentials. |
| EARTH_PULSE_STATE | Optional directory for the local SQLite job/result database. |

Results contain the input geometry and may include selected dates and model explanations. SQLite retains completed measurements locally. A raster layer uses a temporary backend-held Earth Engine tile handle. Rerun the analysis after restarting the backend to regenerate the overlay. Cancellation discards the result of an in-flight provider request. Remote processing already submitted can still consume quota.

## Usage limits

The selector discovers models from the configured endpoint. Verification runs small inference requests and can use provider quota. Local Ollama uses this computer's resources. Hosted Ollama pricing and concurrency depend on the plan. [Ollama pricing](https://ollama.com/pricing)

NVIDIA documents free Developer Program access for prototyping. Check the account's limits and entitlement before using a hosted model. Quota failures appear as provider errors. [NVIDIA FAQ](https://docs.api.nvidia.com/nim/docs/product)

Earth Engine noncommercial access requires verified eligibility. Google lists monthly quotas of 150, 1,000, and 100,000 EECU-hours for Community, Contributor, and Partner projects. Check the project's tier and usage in Google Cloud. Earth Pulse limits one active analysis, raster selections to 1,000 square kilometres, a period to 366 days, and matching scenes to 500 per period. It reuses identical calculations for ten minutes within one backend session. [Earth Engine tiers](https://developers.google.com/earth-engine/guides/noncommercial_tiers)

## Project layout

The EarthCraft desktop source is in `apps/earthcraft`, with reusable libraries in `crates/` and the upstream MIT license at the repository root. `backend/server.py` owns the authenticated HTTP API, jobs and result storage. `backend/providers.py` handles model discovery, probes and inference. `backend/geo.py` owns geometry validation, geodesic measurements and Earth Engine operations.
