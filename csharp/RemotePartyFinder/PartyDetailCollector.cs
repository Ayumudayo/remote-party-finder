using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Linq;
using System.Net.Http;
using System.Net.Http.Headers;
using System.Runtime.InteropServices;
using System.Threading.Tasks;
using Dalamud.Plugin.Services;
using FFXIVClientStructs.FFXIV.Client.UI.Agent;
using Newtonsoft.Json;
using Newtonsoft.Json.Serialization;
using FFXIVClientStructs.FFXIV.Component.GUI;

namespace RemotePartyFinder;

/// <summary>
/// AgentLookingForGroup.Detailed에서 파티 멤버 ContentId를 수집합니다.
/// 사용자가 파티 모집글을 클릭하면 트리거됩니다.
/// </summary>
internal class PartyDetailCollector : IDisposable {
    private Plugin Plugin { get; }
    private HttpClient Client { get; } = new();
    private Stopwatch CheckTimer { get; } = new();
    
    // 이미 업로드한 리스팅을 캐시하여 중복 업로드 방지
    private Dictionary<uint, DateTime> UploadedDetails { get; } = new();
    private const double CacheExpirationMinutes = 0.05; // 3초 (디버깅용)
    private const int CheckIntervalSeconds = 1;

    internal PartyDetailCollector(Plugin plugin) {
        this.Plugin = plugin;
        this.CheckTimer.Start();
        this.Plugin.Framework.Update += this.OnUpdate;
    }

    public void Dispose() {
        this.Plugin.Framework.Update -= this.OnUpdate;
    }

    private unsafe void OnUpdate(IFramework framework) {
        // 체크 주기
        if (this.CheckTimer.Elapsed < TimeSpan.FromSeconds(CheckIntervalSeconds)) {
            return;
        }
        this.CheckTimer.Restart();

        // UI 창(Addon)이 열려있는지 확인
        // GetAddonByName returns 0 if addon is not loaded/visible
        nint addonPtr = this.Plugin.GameGui.GetAddonByName("LookingForGroupDetail", 1);
        if (addonPtr == 0) {
            return;
        }
        // Addon이 로드되어 있으면 진행 (IsVisible 오프셋은 패치마다 다를 수 있어 제거)

        // AgentLookingForGroup 확인
        var agent = AgentLookingForGroup.Instance();
        Plugin.Log.Debug("HERE!! 2");
        if (agent == null) return;

        // Detailed 데이터가 있는지 확인 (LastViewedListing)
        ref var detailed = ref agent->LastViewedListing;
        Plugin.Log.Debug("HERE!! 3");
        if (detailed.ListingId == 0) return;

        // DEBUG: 감지 확인
        Plugin.Log.Debug($"PartyDetailCollector: Found ListingId {detailed.ListingId} Leader {agent->LastLeader}");

        var now = DateTime.UtcNow;

        // 캐시 확인 (최근에 업로드한 리스팅은 스킵)
        if (this.UploadedDetails.TryGetValue(detailed.ListingId, out var lastUpload)) {
            // Plugin.Log.Debug($"PartyDetailCollector: Skipping {detailed.ListingId} (Cached)");
            if ((now - lastUpload).TotalMinutes < CacheExpirationMinutes) return;
        }

        Plugin.Log.Info($"PartyDetailCollector: Processing ListingId {detailed.ListingId} Leader {agent->LastLeader}");

        // 멤버 ContentId 수집
        var memberContentIds = new List<ulong>();
        for (var i = 0; i < detailed.TotalSlots && i < 48; i++) {
            var contentId = detailed.MemberContentIds[i];
            if (contentId != 0) {
                memberContentIds.Add(contentId);
            }
        }

        // 리더 정보
        var leaderContentId = detailed.LeaderContentId;
        var homeWorld = detailed.HomeWorld;
        var leaderName = agent->LastLeader.ToString();

        // DEBUG: 리더 정보 확인
        Plugin.Log.Debug($"PartyDetailCollector: Leader {leaderContentId} Name {leaderName} World {homeWorld}");

        // 유효성 검사
        if (leaderContentId == 0 || homeWorld == 0 || homeWorld >= 1000) return;

        // 업로드 데이터 구성
        var uploadData = new UploadablePartyDetail {
            ListingId = detailed.ListingId,
            LeaderContentId = leaderContentId,
            LeaderName = leaderName,
            HomeWorld = homeWorld,
            MemberContentIds = memberContentIds,
        };
    

        this.UploadedDetails[detailed.ListingId] = now;

        // 오래된 캐시 정리
        var expiredKeys = this.UploadedDetails
            .Where(kvp => (now - kvp.Value).TotalMinutes > CacheExpirationMinutes * 2)
            .Select(kvp => kvp.Key)
            .ToList();
        foreach (var key in expiredKeys) {
            this.UploadedDetails.Remove(key);
        }

        // 서버에 업로드
        UploadDetailAsync(uploadData);
    }

    private void UploadDetailAsync(UploadablePartyDetail detail) {
        Task.Run(async () => {
            try {
                var json = JsonConvert.SerializeObject(detail);
                foreach (var uploadUrl in this.Plugin.Configuration.UploadUrls.Where(u => u.IsEnabled)) {
                    var baseUrl = uploadUrl.Url.TrimEnd('/');

                    if (baseUrl.EndsWith("/contribute/multiple")) {
                        baseUrl = baseUrl.Substring(0, baseUrl.Length - "/contribute/multiple".Length);
                    } else if (baseUrl.EndsWith("/contribute")) {
                        baseUrl = baseUrl.Substring(0, baseUrl.Length - "/contribute".Length);
                    }

                    var detailUrl = baseUrl + "/contribute/detail";
                    
                    var resp = await this.Client.PostAsync(detailUrl, new StringContent(json) {
                        Headers = { ContentType = MediaTypeHeaderValue.Parse("application/json") },
                    });
                    var output = await resp.Content.ReadAsStringAsync();
                    Plugin.Log.Debug($"PartyDetailCollector: {detailUrl}: {output}");
                }
            } catch (Exception e) {
                Plugin.Log.Error($"PartyDetailCollector upload error: {e.Message}");
            }
        });
    }
}

[Serializable]
[JsonObject(NamingStrategyType = typeof(SnakeCaseNamingStrategy))]
internal class UploadablePartyDetail {
    public uint ListingId { get; set; }
    public ulong LeaderContentId { get; set; }
    public string LeaderName { get; set; } = string.Empty;
    public ushort HomeWorld { get; set; }
    public List<ulong> MemberContentIds { get; set; } = new();
}
