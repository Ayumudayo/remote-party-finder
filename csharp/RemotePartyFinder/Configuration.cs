using System;
using System.Collections.Immutable;
using Dalamud.Configuration;

namespace RemotePartyFinder;

[Serializable]
public class Configuration : IPluginConfiguration {
    public int Version { get; set; } = 1;
    public bool AdvancedSettingsEnabled = false;
    public ImmutableList<UploadUrl> UploadUrls = DefaultUploadUrls();

    public static ImmutableList<UploadUrl> DefaultUploadUrls() => [
        new("http://127.0.0.1:8000") { IsDefault = true }
    ];

    public void Save() {
        Plugin.PluginInterface.SavePluginConfig(this);
    }
}
