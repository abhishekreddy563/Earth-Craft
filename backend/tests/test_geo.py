import copy
import os
import unittest
from unittest.mock import MagicMock, patch

from backend import geo


def polygon(*points):
    return {"geometry": {"type": "Polygon", "coordinates": [[*points, points[0]]]}}


class GeographyTests(unittest.TestCase):
    def test_equatorial_measurement_crosses_date_line(self):
        context = {"geometry": {"type": "LineString", "coordinates": [[179.5, 0], [-179.5, 0]]}}
        result = geo.run_tool("distance", context)
        self.assertAlmostEqual(result["statistics"]["distance_km"], 111.319490793, places=7)

    def test_unit_square_matches_geodesic_reference_in_both_directions(self):
        ring = [[0, 0], [1, 0], [1, 1], [0, 1]]
        for corners in [ring, ring[::-1]]:
            result = geo.run_tool("area", polygon(*corners))
            self.assertAlmostEqual(result["statistics"]["area_km2"], 12308.778361, places=5)
            self.assertAlmostEqual(result["statistics"]["perimeter_km"], 443.770917, places=5)

    def test_validates_geometry_before_any_network_access(self):
        point = [0, 0]
        invalid = [
            polygon(point, [1, 1], [0, 1], [1, 0]),
            polygon(point, [1, 0], [2, 0]),
            polygon(point, [1, 91], [2, 0]),
            polygon(point, [True, 1], [2, 0]),
            polygon(point, [float("nan"), 1], [2, 0]),
            polygon([-179, 0], [179, 0], [-179, 1]),
            {"geometry": {"type": "Polygon", "coordinates": [[[0, 0], [1, 1], [1, 0]]]}},
        ]
        with patch.object(geo, "_earth_engine", side_effect=AssertionError("unexpected network")):
            for context in invalid:
                with self.subTest(context=context), self.assertRaises(geo.GeoError):
                    geo.run_tool("area", context)
            with self.assertRaisesRegex(geo.GeoError, "1,000"):
                geo.run_tool("ndvi_summary", polygon(point, [1, 0], [1, 1]))

    def test_dates_and_comparison_order_are_checked_before_computing(self):
        context = polygon([0, 0], [.01, 0], [0, .01])
        context.update(start="2023-01-01", end="2023-01-31",
                       compare_start="2023-01-31", compare_end="2023-02-05")
        with patch.object(geo, "_earth_engine", side_effect=AssertionError("unexpected network")):
            with self.assertRaisesRegex(geo.GeoError, "without overlap"):
                geo.run_tool("ndvi_change", context)
            with self.assertRaisesRegex(geo.GeoError, "366"):
                geo.run_tool("ndvi_summary", dict(context, end="2025-01-01"))
            with self.assertRaisesRegex(geo.GeoError, "YYYY-MM-DD"):
                geo.run_tool("ndvi_summary", dict(context, start="bad"))

    def test_no_scenes_has_different_status_from_all_masked(self):
        api = MagicMock()
        collection = api.ImageCollection.return_value.filterBounds.return_value.filterDate.return_value
        collection.limit.return_value.aggregate_array.return_value.getInfo.return_value = []
        first, last = geo.date(2023, 1, 1), geo.date(2023, 1, 31)
        with self.assertRaisesRegex(geo.GeoError, "No Sentinel-2 scenes"):
            geo._composite(api, object(), (first, last))
        api.ImageCollection.return_value.filterBounds.return_value.filterDate.assert_called_with(
            "2023-01-01", "2023-02-01")
        output = api.Image.constant.return_value.rename.return_value.addBands.return_value
        output.reduceRegion.return_value.getInfo.return_value = {"total_count": 100, "value_count": 0}
        with self.assertRaisesRegex(geo.GeoError, "No valid pixels"):
            geo._reduce(api, MagicMock(), object())

    def test_missing_ee_project_does_not_affect_local_tools(self):
        context = polygon([0, 0], [0, .01], [.01, .01])
        original = copy.deepcopy(context)
        with patch.dict(os.environ, {"EE_PROJECT": ""}):
            self.assertFalse(geo.earth_engine_status(True)["available"])
            with self.assertRaisesRegex(geo.GeoError, "EE_PROJECT"):
                geo.run_tool("elevation_summary", context)
            self.assertGreater(geo.run_tool("area", context)["statistics"]["area_km2"], 0)
        self.assertEqual(context, original)

    def test_status_remembers_successful_verification_for_same_project_only(self):
        with (
            patch.dict(os.environ, {"EE_PROJECT": "test-project"}),
            patch.object(geo, "_init_ee", return_value=object()) as initialize,
            patch.object(geo, "_VERIFIED_PROJECT", None),
        ):
            self.assertFalse(geo.earth_engine_status()["available"])
            self.assertTrue(geo.earth_engine_status(verify=True)["available"])
            self.assertTrue(geo.earth_engine_status()["available"])
            initialize.assert_called_once_with("test-project")
            with patch.dict(os.environ, {"EE_PROJECT": "different-project"}):
                self.assertFalse(geo.earth_engine_status()["available"])


if __name__ == "__main__":
    unittest.main()
