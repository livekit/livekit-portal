from hatchling.builders.hooks.plugin.interface import BuildHookInterface


class CustomBuildHook(BuildHookInterface):
    PLUGIN_NAME = "custom"

    def initialize(self, version, build_data):
        if self.target_name == "wheel":
            # The wheel contains a platform-specific cdylib loaded via ctypes.
            # Hatchling does not detect ctypes libraries as native extensions,
            # so it would tag the wheel py3-none-any. Force the correct tag.
            build_data["pure_python"] = False
            build_data["infer_tag"] = True
