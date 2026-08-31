from dlss5oneclick import reshade_ini as ri


def test_split_join_roundtrip_with_escaped_comma():
    items = ["A=1", "B=x,,y", "C"]
    raw = ri.join_list(["A=1", "B=x,y", "C"])
    assert raw == "A=1,B=x,,y,C"
    assert ri.split_list(raw) == ["A=1", "B=x,y", "C"]
    assert ri.split_list("") == []
    del items


def test_write_reshade_ini_fresh(tmp_path):
    p = ri.write_reshade_ini(tmp_path)
    data = ri.parse(p.read_text())
    g = data["GENERAL"]
    assert g["EffectSearchPaths"] == r".\reshade-shaders\Shaders\**"
    assert g["TextureSearchPaths"] == r".\reshade-shaders\Textures\**"
    assert g["PresetPath"] == r".\ReShadePreset.ini"
    assert g["PreprocessorDefinitions"] == "DLSS5_MV_PROVIDER=3"


def test_write_reshade_ini_preserves_user_keys_and_replaces_define(tmp_path):
    (tmp_path / "ReShade.ini").write_text(
        "[GENERAL]\nEffectSearchPaths=.\\custom\\**\n"
        "PreprocessorDefinitions=FOO=1,DLSS5_MV_PROVIDER=5\n"
        "[INPUT]\nKeyOverlay=36,0,0,0\n")
    ri.write_reshade_ini(tmp_path)
    data = ri.parse((tmp_path / "ReShade.ini").read_text())
    assert data["GENERAL"]["EffectSearchPaths"] == ".\\custom\\**"
    assert ri.split_list(data["GENERAL"]["PreprocessorDefinitions"]) == [
        "FOO=1", "DLSS5_MV_PROVIDER=3"]
    assert data["INPUT"]["KeyOverlay"] == "36,0,0,0"


def test_write_preset_fresh(tmp_path):
    p = ri.write_preset(tmp_path)
    data = ri.parse(p.read_text())
    assert ri.split_list(data[""]["Techniques"]) == ri.TECHNIQUES_ORDERED
    assert data[""]["PreprocessorDefinitions"] == "DLSS5_MV_PROVIDER=3"


def test_write_preset_keeps_provider_above_feed_and_user_techniques(tmp_path):
    (tmp_path / "ReShadePreset.ini").write_text(
        "Techniques=DLSS5_Feed@DLSS5_Feed.fx,Clarity@Clarity.fx\n"
        "TechniqueSorting=Clarity@Clarity.fx,DLSS5_Feed@DLSS5_Feed.fx\n"
        "[Clarity.fx]\nStrength=0.5\n")
    ri.write_preset(tmp_path)
    data = ri.parse((tmp_path / "ReShadePreset.ini").read_text())
    assert ri.split_list(data[""]["Techniques"]) == [
        "Lumenite_Kernel@lumenite_Kernel.fx", "DLSS5_Feed@DLSS5_Feed.fx", "Clarity@Clarity.fx"]
    assert ri.split_list(data[""]["TechniqueSorting"])[:2] == ri.TECHNIQUES_ORDERED
    assert data["Clarity.fx"]["Strength"] == "0.5"
