"""Earth Pulse backend tools. The desktop supplies the geometry, never the model."""
from __future__ import annotations

import math
import os
import threading
from datetime import date, datetime, timedelta, timezone
from functools import lru_cache

from geographiclib.geodesic import Geodesic


class GeoError(ValueError):
    """Safe, actionable analysis error for the user."""


_DESCRIPTIONS = {
    "distance": "Measure WGS84 surface distance along the selected line.",
    "area": "Measure WGS84 area and perimeter of the selected polygon.",
    "ndvi_summary": "Summarize vegetation greenness within the polygon for the selected dates.",
    "ndvi_change": "Compare vegetation greenness in two selected date ranges on common valid pixels.",
    "elevation_summary": "Summarize SRTM terrain elevation within the polygon.",
}
_EE_TOOLS = {"ndvi_summary", "ndvi_change", "elevation_summary"}
_SENTINEL = "COPERNICUS/S2_SR_HARMONIZED"
_ELEVATION = "USGS/SRTMGL1_003"
_SCALE_M = 30
_CRS = "EPSG:6933"  # Equal-area grid; restrict analyses to its published coverage.
_MAX_AREA_M2 = 1_000_000_000
_MAX_VERTICES = 256
_MAX_DAYS = 366
_MAX_PIXELS = 3_000_000
_INIT_LOCK = threading.Lock()
_VERIFIED_PROJECT = None


def tools():
    return [
        {"name": name, "description": description, "parameters": {
            "type": "object", "properties": {}, "additionalProperties": False}}
        for name, description in _DESCRIPTIONS.items()
    ]


def _point(value):
    if not isinstance(value, (list, tuple)) or len(value) != 2:
        raise GeoError("Use longitude, latitude coordinate pairs.")
    if any(type(n) not in (int, float) or not -180 <= n <= 180 for n in value):
        raise GeoError("Coordinates must be finite WGS84 longitude and latitude values.")
    lon, lat = value
    if not -90 <= lat <= 90:
        raise GeoError("Latitude must be between -90 and 90 degrees.")
    return (float(lon), float(lat))


def _intersect(a, b, c, d):
    def cross(p, q, r):
        return (q[0] - p[0]) * (r[1] - p[1]) - (q[1] - p[1]) * (r[0] - p[0])

    def within(p, a, b):
        return all(min(x, y) <= z <= max(x, y) for x, y, z in zip(a, b, p))

    u, v, w, t = cross(a, b, c), cross(a, b, d), cross(c, d, a), cross(c, d, b)
    return (u * v < 0 and w * t < 0 or u == 0 and within(c, a, b)
            or v == 0 and within(d, a, b) or w == 0 and within(a, c, d)
            or t == 0 and within(b, c, d))


def _geometry(context, kind):
    geom = context.get("geometry")
    if not isinstance(geom, dict) or geom.get("type") != kind:
        raise GeoError(f"Select a {kind} on the globe for this analysis.")
    coords = geom.get("coordinates")
    if kind == "Polygon":
        if not isinstance(coords, list) or len(coords) != 1:
            raise GeoError("Select one polygon with a single outer ring.")
        coords = coords[0]
    if not isinstance(coords, list) or not 2 <= len(coords) <= _MAX_VERTICES:
        raise GeoError(f"Select between 2 and {_MAX_VERTICES} coordinates.")
    points = [_point(p) for p in coords]
    if kind == "Polygon":
        if len(points) < 4 or points[0] != points[-1] or len(set(points[:-1])) != len(points)-1:
            raise GeoError("Draw a closed polygon with at least three distinct corners.")
        if max(p[0] for p in points) - min(p[0] for p in points) >= 180:
            raise GeoError("Split polygons that cross the antimeridian or span a hemisphere.")
        segments = list(zip(points, points[1:]))
        for i, (a, b) in enumerate(segments):
            for j in range(i + 2, len(segments)):
                if (i != 0 or j != len(segments)-1) and _intersect(a, b, *segments[j]):
                    raise GeoError("The polygon crosses itself. Draw a simple boundary.")
    normalized = [list(p) for p in points]
    return {"type": kind, "coordinates": [normalized] if kind == "Polygon" else normalized}, points


def _polygon_metrics(points):
    polygon = Geodesic.WGS84.Polygon()
    for lon, lat in points[:-1]:
        polygon.AddPoint(lat, lon)
    _, perimeter, signed_area = polygon.Compute()
    area = abs(signed_area)
    if area < 0.01:
        raise GeoError("The selected polygon has negligible area.")
    return perimeter, area


def _window(context, prefix=""):
    try:
        first, last = (date.fromisoformat(context[prefix + field]) for field in ("start", "end"))
        if any(context[prefix + k] != v.isoformat() for k, v in (("start", first), ("end", last))):
            raise ValueError()
    except (ValueError, KeyError, TypeError):
        raise GeoError("Choose a start and end date using YYYY-MM-DD.") from None
    if not 0 <= (last - first).days < _MAX_DAYS:
        raise GeoError("Choose an ordered date range of at most 366 days.")
    if last > datetime.now(timezone.utc).date():
        raise GeoError("Choose observation dates through today.")
    return first, last


def _safe_ee_error(exc):
    msg = str(exc).lower()
    if any(term in msg for term in ("quota", "429", "rate limit", "resource_exhausted")):
        return GeoError("Earth Engine quota or request limit reached. Check your project quota and retry later.")
    if any(term in msg for term in ("permission", "credential", "authenticated", "403", "401", "authorize")):
        return GeoError("Earth Engine access failed. Check backend credentials and project registration.")
    return GeoError("Earth Engine request failed. Check the project, connectivity and computation limits.")


@lru_cache(maxsize=1)
def _init_ee(project):
    try:
        import ee
        import google.auth
        credentials, _ = google.auth.default(scopes=["https://www.googleapis.com/auth/cloud-platform"])
        ee.Initialize(credentials=credentials, project=project)
        ee.data.setDeadline(60_000)
        ee.data.setMaxRetries(1)
        ee.Number(1).getInfo()
        return ee
    except ImportError:
        raise GeoError("Install the backend Earth Engine dependencies.") from None
    except Exception as exc:
        raise _safe_ee_error(exc) from None


def _earth_engine():
    global _VERIFIED_PROJECT
    project = os.environ.get("EE_PROJECT", "").strip()
    if not project:
        raise GeoError("Set EE_PROJECT and configure Google Application Default Credentials on the backend.")
    with _INIT_LOCK:
        api = _init_ee(project)
        _VERIFIED_PROJECT = project
        return api


def earth_engine_status(verify=False):
    project = os.environ.get("EE_PROJECT", "").strip()
    if not project:
        return {"available": False, "status": "EE_PROJECT is unset."}
    if not verify:
        if project == _VERIFIED_PROJECT:
            return {"available": True, "status": "Earth Engine computation verified this session; account quota unknown."}
        return {"available": False, "status": "Configured; access requires a live check."}
    try:
        _earth_engine()
    except GeoError as exc:
        return {"available": False, "status": str(exc)}
    return {"available": True, "status": "Earth Engine computation verified; account quota unknown."}


def _composite(ee, region, window):
    first, last = window
    scenes = ee.ImageCollection(_SENTINEL).filterBounds(region).filterDate(
        first.isoformat(), (last + timedelta(days=1)).isoformat())
    # Bound the number of scenes before collecting metadata or computing a median.
    times = scenes.limit(501).aggregate_array("system:time_start").getInfo()
    if not times:
        raise GeoError(f"No Sentinel-2 scenes intersect this region from {first} through {last}.")
    if len(times) > 500:
        raise GeoError("More than 500 scenes match. Select a shorter period or smaller region.")

    def ndvi(image):
        scl = image.select("SCL")
        # Classes 4, 5 and 6: vegetation, bare surfaces, and water.
        clear = scl.eq(4).Or(scl.eq(5)).Or(scl.eq(6))
        return image.normalizedDifference(["B8", "B4"]).updateMask(clear).rename("value")

    dates = sorted({datetime.fromtimestamp(t / 1000, timezone.utc).date().isoformat() for t in times})
    return scenes.map(ndvi).median(), {
        "start": first.isoformat(), "end_inclusive": last.isoformat(), "scene_count": len(times),
        "scene_dates_utc": dates,
    }


def _reduce(ee, image, region):
    # The global constant also counts samples outside the image footprint. A masked
    # portion of an AOI must not silently disappear from its coverage denominator.
    counts = ee.Image.constant(1).rename("total").addBands(image.rename("value"))
    reducer = ee.Reducer.minMax().combine(ee.Reducer.median(), sharedInputs=True).combine(
        ee.Reducer.count(), sharedInputs=True)
    output = counts.reduceRegion(reducer=reducer, geometry=region, crs=_CRS, scale=_SCALE_M,
                                 maxPixels=_MAX_PIXELS).getInfo()
    total, valid = output.get("total_count"), output.get("value_count")
    if not total:
        raise GeoError("No 30 m sample centers fall inside this polygon. Select a larger region.")
    if not valid or output.get("value_median") is None:
        raise GeoError("No valid pixels remain in the region at this scale after masking or coverage filtering.")
    return {
        "minimum": output["value_min"], "median": output["value_median"], "maximum": output["value_max"],
        "valid_sample_count": valid, "region_sample_count": total, "coverage_percent": 100 * valid / total,
    }


def _raster(tool, context, geometry, points, area):
    if area > _MAX_AREA_M2 or max(abs(lat) for lon, lat in points) >= 86:
        raise GeoError("Select a region of at most 1,000 km² within 86° of the equator.")
    window = _window(context) if tool != "elevation_summary" else None
    later = _window(context, "compare_") if tool == "ndvi_change" else None
    if later and later[0] <= window[1]:
        raise GeoError("Choose the comparison period after the first period, without overlap.")
    ee = _earth_engine()
    region = ee.Geometry.Polygon(geometry["coordinates"], proj="EPSG:4326", geodesic=True)
    provenance = {"engine": "Google Earth Engine", "crs": _CRS, "scale_m": _SCALE_M,
                  "region_area_km2": area / 1e6}
    if tool == "elevation_summary":
        image = ee.Image(_ELEVATION).select("elevation")
        provenance.update(dataset=_ELEVATION, units="m", attribution="NASA / USGS / JPL-Caltech",
                          method="SRTM v3 elevation sampled on a 30 m equal-area grid.",
                          acquisition_period="2000-02-11 to 2000-02-22")
        viz = {"min": 0, "max": 4500, "palette": ["264653", "a3b18a", "bc6c25", "ffffff"]}
    else:
        image, period = _composite(ee, region, window)
        provenance.update(dataset=_SENTINEL, units="unitless NDVI", attribution="Copernicus Sentinel-2",
                          periods=[period], method="Per-scene (B8-B4)/(B8+B4), SCL classes 4, 5, 6 only; "
                          "per-pixel temporal median followed by regional summary at 30 m.")
        viz = {"min": -0.2, "max": 0.9, "palette": ["276fbf", "e9c46a", "87b548", "16562e"]}
        if later:
            second, period = _composite(ee, region, later)
            image = second.subtract(image)
            provenance["periods"].append(period)
            provenance["method"] += " Change is later minus earlier on common valid pixels."
            viz = {"min": -0.5, "max": 0.5, "palette": ["ab292d", "f7f7f7", "16733c"]}
    stats = _reduce(ee, image, region)
    warnings = []
    if stats["valid_sample_count"] < stats["region_sample_count"]:
        warnings.append("Statistics apply only to valid samples. See coverage_percent.")
    result = {"tool": tool, "geometry": geometry, "statistics": stats, "provenance": provenance,
              "warnings": warnings}
    try:
        tile_fetcher = image.clip(region).getMapId({**viz, "format":"png"})["tile_fetcher"]
    except Exception:
        warnings.append("Statistics completed, but Earth Engine could not prepare the raster layer.")
    else:
        result.update(layer={"name": tool, **viz}, _tile_fetcher=tile_fetcher)
    return result


def run_tool(name, context):
    if not isinstance(name, str) or name not in _DESCRIPTIONS:
        raise GeoError("Unknown analysis tool.")
    if not isinstance(context, dict):
        raise GeoError("Select a region on the globe.")
    geometry, points = _geometry(context, "LineString" if name == "distance" else "Polygon")
    if name == "distance":
        length = math.fsum(Geodesic.WGS84.Inverse(a[1], a[0], b[1], b[0])["s12"]
                           for a, b in zip(points, points[1:]))
        stats = {"distance_km": length / 1000}
        method = "Sum of shortest WGS84 ellipsoid geodesics between successive points."
    else:
        perimeter, area = _polygon_metrics(points)
        if name in _EE_TOOLS:
            try:
                return _raster(name, context, geometry, points, area)
            except GeoError:
                raise
            except Exception as exc:
                raise _safe_ee_error(exc) from None
        stats = {"area_km2": area / 1e6, "perimeter_km": perimeter / 1000}
        method = "Area and perimeter enclosed by WGS84 ellipsoid geodesic edges."
    return {"tool": name, "geometry": geometry, "statistics": stats, "warnings": [],
            "provenance": {"engine": "GeographicLib", "source": "User-selected WGS84 geometry",
                           "method": method, "crs": "EPSG:4326"}}
