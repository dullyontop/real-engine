//! `DataStoreService` and `MemoryStoreService` over project files
//! (ADR-0048), through the headless harness. `cargo test -p re_compat data`
//! runs them.

use std::path::Path;

use re_compat::CompatSetup;
use re_compat::scene::create_place;
use re_compat::testing::{Harness, HarnessOptions, Side};
use re_scene::{SaveOptions, World};

/// Writes an empty compat project to `dir`; `toml` goes after `[compat]`.
fn project(dir: &Path, toml: &str) {
    let setup = CompatSetup::standard().unwrap();
    let mut world = World::new();
    create_place(
        &mut world,
        &setup.classes,
        &["Workspace", "ServerScriptService", "ReplicatedStorage"],
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("scenes")).unwrap();
    std::fs::write(
        dir.join("scenes/main.scene.json"),
        re_scene::scene::to_json(&world.save(SaveOptions::default())),
    )
    .unwrap();
    std::fs::write(
        dir.join("real.toml"),
        format!(
            "[project]\nname = \"t\"\nentry_scene = \"scenes/main.scene.json\"\n\n[scripts]\ncpu_budget_ms = 1000\n\n[compat]\napi = \"roblox\"\n{toml}"
        ),
    )
    .unwrap();
}

fn open(dir: &Path, persist: bool) -> Harness {
    let opts = HarnessOptions {
        persist_data: persist,
        ..HarnessOptions::default()
    };
    let mut h = Harness::open(dir, opts).unwrap();
    let d = h.start();
    assert!(d.is_empty(), "{d:#?}");
    h
}

/// Runs `source` on `side`, steps `frames`, and returns the printed lines
/// (warnings start with `W `).
fn run(h: &mut Harness, side: Side, source: &str, frames: u32) -> Vec<String> {
    let d = h.exec(side, "test", source);
    assert!(d.is_empty(), "{d:#?}");
    let d = h.step(frames);
    assert!(d.is_empty(), "{d:#?}");
    assert!(h.uncaught().is_empty(), "{:#?}", h.uncaught());
    h.take_output(side)
        .into_iter()
        .map(|l| {
            if l.warn {
                format!("W {}", l.text)
            } else {
                l.text
            }
        })
        .collect()
}

mod data {
    use super::*;

    #[test]
    fn data_stores_get_set_increment_update_and_remove() {
        let dir = tempfile::tempdir().unwrap();
        project(dir.path(), "");
        let mut h = open(dir.path(), false);
        let out = run(
            &mut h,
            Side::Server,
            r#"
            local DSS = game:GetService("DataStoreService")
            local store = DSS:GetDataStore("Coins")
            print(store.ClassName, store.Name, store == DSS:GetDataStore("Coins"))
            print(store:GetAsync("p1"))
            local opts = Instance.new("DataStoreSetOptions")
            opts:SetMetadata({ source = "test" })
            print(opts:GetMetadata().source)
            print(store:SetAsync("p1", { coins = 5, items = { "a", "b" } }, { 7 }, opts))
            local v, info = store:GetAsync("p1")
            print(v.coins, v.items[2], info.Version, info.CreatedTime == info.UpdatedTime)
            print(info:GetUserIds()[1], info:GetMetadata().source)
            local n, i2 = store:IncrementAsync("count", 3)
            print(n, i2.Version, (store:IncrementAsync("count")))
            local nv, ui = store:UpdateAsync("p1", function(old, oldInfo)
                old.coins += 10
                return old, oldInfo:GetUserIds()
            end)
            print(nv.coins, ui.Version, ui:GetUserIds()[1])
            print(store:UpdateAsync("p1", function() return nil end))
            local old, ri = store:RemoveAsync("count")
            print(old, ri.Version, (store:GetAsync("count")))
            print(store:GetVersionAsync("p1", "2").coins, (store:GetVersionAsync("p1", "1")))
            local versions = store:ListVersionsAsync("p1"):GetCurrentPage()
            print(#versions, versions[1].Version)
            local g = DSS:GetGlobalDataStore()
            g:SetAsync("motd", "hi")
            print(g.ClassName, (g:GetAsync("motd")))
            print(pcall(store.SetAsync, store, "bad", function() end))
            print(pcall(store.SetAsync, store, "", 1))
            "#,
            30,
        );
        assert_eq!(
            &out[..13],
            [
                "DataStore\tCoins\ttrue",
                "nil\tnil",
                "test",
                "1",
                "5\tb\t1\ttrue",
                "7\ttest",
                "3\t1\t4",
                "15\t2\t7",
                "nil",
                "4\t2\tnil",
                "15\tnil",
                "1\t2",
                "GlobalDataStore\thi",
            ],
            "{out:#?}"
        );
        assert_eq!(out.len(), 15, "{out:#?}");
        assert!(
            out[13].starts_with("false\t104: ") && out[13].ends_with("[RE9400]"),
            "{out:?}"
        );
        assert!(
            out[14].starts_with("false\t101: ") && out[14].ends_with("[RE9400]"),
            "{out:?}"
        );
    }

    #[test]
    fn keys_stores_and_ordered_stores_list_in_pages() {
        let dir = tempfile::tempdir().unwrap();
        project(dir.path(), "");
        let mut h = open(dir.path(), false);
        let out = run(
            &mut h,
            Side::Server,
            r#"
            local DSS = game:GetService("DataStoreService")
            local store = DSS:GetDataStore("Keys")
            for i = 1, 5 do store:SetAsync("k" .. i, i) end
            store:SetAsync("other", 0)
            local pages = store:ListKeysAsync("k", 2)
            local seen = {}
            while true do
                for _, k in pages:GetCurrentPage() do table.insert(seen, k.KeyName) end
                if pages.IsFinished then break end
                pages:AdvanceToNextPageAsync()
            end
            print(table.concat(seen, ","))
            local stores = DSS:ListDataStoresAsync():GetCurrentPage()
            print(#stores, stores[1].DataStoreName)
            local board = DSS:GetOrderedDataStore("Board")
            board:SetAsync("ann", 30)
            board:SetAsync("bob", 10)
            board:IncrementAsync("cat", 20)
            print(pcall(board.SetAsync, board, "dan", "x"))
            local top = board:GetSortedAsync(false, 2):GetCurrentPage()
            print(top[1].key, top[1].value, top[2].key, #top)
            local low = board:GetSortedAsync(true, 10, 15, 40):GetCurrentPage()
            print(low[1].key, #low)
            print(DSS:GetRequestBudgetForRequestType(Enum.DataStoreRequestType.GetAsync) > 0)
            print(DSS:GetRequestBudgetForRequestType(Enum.DataStoreRequestType.SetIncrementSortedAsync) > 0)
            "#,
            60,
        );
        assert_eq!(out[0], "k1,k2,k3,k4,k5", "{out:#?}");
        assert_eq!(out[1], "1\tKeys", "{out:#?}");
        assert!(out[2].starts_with("false\t104: "), "{out:#?}");
        assert_eq!(out[3], "ann\t30\tcat\t2", "{out:#?}");
        assert_eq!(out[4], "cat\t2", "{out:#?}");
        assert_eq!(&out[5..], ["true", "true"], "{out:#?}");
    }

    #[test]
    fn a_request_over_budget_waits_and_a_full_queue_drops_it() {
        let dir = tempfile::tempdir().unwrap();
        project(dir.path(), "");
        let mut h = open(dir.path(), false);
        let out = run(
            &mut h,
            Side::Server,
            r#"
            local store = game:GetService("DataStoreService"):GetDataStore("Busy")
            local ok, dropped = 0, 0
            for i = 1, 100 do
                task.spawn(function()
                    local good, err = pcall(store.SetAsync, store, "k" .. i, i)
                    if good then ok += 1 elseif string.find(err, "RE9401", 1, true) then dropped += 1 end
                end)
            end
            task.wait(0.5)
            print(ok, dropped)
            "#,
            40,
        );
        // 60 writes a minute start available, 30 more wait in the queue.
        assert_eq!(out, ["60\t10"], "{out:#?}");
    }

    #[test]
    fn data_persists_across_runs_only_when_asked() {
        let dir = tempfile::tempdir().unwrap();
        project(dir.path(), "");
        let write = r#"
            game:GetService("DataStoreService"):GetDataStore("Save", "slot"):SetAsync("Key", 42)
            local q = game:GetService("MemoryStoreService"):GetHashMap("Live")
            q:SetAsync("a", 1, 60)
            print("saved")
        "#;
        let read = r#"
            local DSS = game:GetService("DataStoreService")
            print((DSS:GetDataStore("Save", "slot"):GetAsync("Key")))
            print(game:GetService("MemoryStoreService"):GetHashMap("Live"):GetAsync("a"))
        "#;
        let mut h = open(dir.path(), true);
        assert_eq!(run(&mut h, Side::Server, write, 10), ["saved"]);
        let file = dir.path().join(".real/data/datastore/_^save/_slot.json");
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("real-engine/datastore"), "{text}");
        assert!(text.contains("\"Key\""), "{text}");
        assert!(
            dir.path()
                .join(".real/data/memory/hash/_^live.json")
                .exists()
        );
        let mut h = open(dir.path(), true);
        assert_eq!(run(&mut h, Side::Server, read, 10), ["42", "1"]);
        // Without persistence the run starts from data/seed/, which is empty.
        let mut h = open(dir.path(), false);
        assert_eq!(run(&mut h, Side::Server, read, 10), ["nil", "nil"]);
        // A seed file is read and never written.
        let seed = dir.path().join("data/seed/datastore/_^save");
        std::fs::create_dir_all(&seed).unwrap();
        std::fs::copy(&file, seed.join("_slot.json")).unwrap();
        let mut h = open(dir.path(), false);
        let out = run(
            &mut h,
            Side::Server,
            r#"
            local s = game:GetService("DataStoreService"):GetDataStore("Save", "slot")
            print((s:GetAsync("Key")))
            s:SetAsync("Key", 7)
            print((s:GetAsync("Key")))
            "#,
            10,
        );
        assert_eq!(out, ["42", "7"]);
        assert_eq!(
            std::fs::read_to_string(seed.join("_slot.json")).unwrap(),
            text
        );
    }

    #[test]
    fn an_unreadable_file_is_set_aside_with_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        project(dir.path(), "");
        let store = dir.path().join(".real/data/datastore/_^broken");
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(store.join("_global.json"), "{ nope").unwrap();
        let mut h = open(dir.path(), true);
        let out = run(
            &mut h,
            Side::Server,
            r#"print(game:GetService("DataStoreService"):GetDataStore("Broken"):GetAsync("x"))"#,
            10,
        );
        assert_eq!(out.len(), 2, "{out:#?}");
        assert!(out[0].starts_with("W [RE9402] 501: "), "{out:#?}");
        assert_eq!(out[1], "nil\tnil");
        assert!(store.join("_global.json.bad").exists());
    }

    #[test]
    fn clients_and_disabled_projects_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        project(
            dir.path(),
            "\n[compat.data]\nenabled = false\nlatency_frames = 2\n",
        );
        let mut h = open(dir.path(), false);
        let call = r#"
            local DSS = game:GetService("DataStoreService")
            print(pcall(DSS.GetDataStore, DSS, "x"))
            local MSS = game:GetService("MemoryStoreService")
            print(pcall(MSS.GetQueue, MSS, "q"))
        "#;
        let out = run(&mut h, Side::Server, call, 2);
        assert!(
            out[0].starts_with("false\t403: ") && out[0].ends_with("[RE9400]"),
            "{out:?}"
        );
        assert!(out[1].starts_with("false\t403: "), "{out:?}");
        let out = run(&mut h, Side::Client, call, 2);
        assert!(out[0].contains("only be used from the server"), "{out:?}");
        assert!(out[1].contains("only be used from the server"), "{out:?}");
    }

    #[test]
    fn memory_store_queues_hold_read_items_until_removed() {
        let dir = tempfile::tempdir().unwrap();
        project(dir.path(), "");
        let mut h = open(dir.path(), false);
        let out = run(
            &mut h,
            Side::Server,
            r#"
            local MSS = game:GetService("MemoryStoreService")
            local q = MSS:GetQueue("Jobs", 5)
            print(q.ClassName, q == MSS:GetQueue("Jobs", 5))
            q:AddAsync("low", 60)
            q:AddAsync("high", 60, 10)
            q:AddAsync({ n = 3 }, 60)
            print(q:GetSizeAsync())
            local items, id = q:ReadAsync(2)
            print(#items, items[1], items[2], typeof(id))
            print(q:GetSizeAsync(), q:GetSizeAsync(true))
            q:RemoveAsync(id)
            print(q:GetSizeAsync())
            local none = q:ReadAsync(5, true, 0)
            print(#none)
            local rest = q:ReadAsync(5, false, 0)
            print(#rest, rest[1].n)
            print(#q:ReadAsync(1, false, 0))
            task.wait(6)
            local again = q:ReadAsync(1, false, 0)
            print(#again, again[1].n)
            print(pcall(q.AddAsync, q, string.rep("x", 40000)))
            "#,
            420,
        );
        assert_eq!(
            &out[..9],
            [
                "MemoryStoreQueue\ttrue",
                "3",
                "2\thigh\tlow\tstring",
                "3\t1",
                "1",
                "0",
                "1\t3",
                "0",
                "1\t3",
            ],
            "{out:#?}"
        );
        assert!(
            out[9].starts_with("false\tItemValueSizeTooLarge: ") && out[9].ends_with("[RE9403]"),
            "{out:#?}"
        );
    }

    #[test]
    fn a_queue_read_waits_for_an_item() {
        let dir = tempfile::tempdir().unwrap();
        project(dir.path(), "");
        let mut h = open(dir.path(), false);
        let out = run(
            &mut h,
            Side::Server,
            r#"
            local q = game:GetService("MemoryStoreService"):GetQueue("Wait")
            task.spawn(function()
                local items = q:ReadAsync(1, false, 10)
                print("read", items[1])
            end)
            task.wait(3)
            print("adding")
            q:AddAsync("job")
            "#,
            400,
        );
        assert_eq!(out, ["adding", "read\tjob"], "{out:#?}");
    }

    #[test]
    fn sorted_maps_order_by_sort_key_then_key() {
        let dir = tempfile::tempdir().unwrap();
        project(dir.path(), "");
        let mut h = open(dir.path(), false);
        let out = run(
            &mut h,
            Side::Server,
            r#"
            local m = game:GetService("MemoryStoreService"):GetSortedMap("Board")
            print(m:SetAsync("b", "B", 60, 2), m:SetAsync("b", "B2", 60, 2))
            m:SetAsync("a", "A", 60, 5)
            m:SetAsync("c", "C", 60, "word")
            m:SetAsync("d", "D", 60)
            print(m:GetAsync("b"))
            print(m:GetSizeAsync())
            local function keys(rows)
                local t = {}
                for _, r in rows do table.insert(t, r.key) end
                return table.concat(t, ",")
            end
            print(keys(m:GetRangeAsync(Enum.SortDirection.Ascending, 10)))
            print(keys(m:GetRangeAsync(Enum.SortDirection.Descending, 2)))
            print(keys(m:GetRangeAsync(Enum.SortDirection.Ascending, 10, { sortKey = 2, key = "b" })))
            print(m:UpdateAsync("a", function(v, sk) return v .. "!", sk + 1 end, 60))
            m:RemoveAsync("d")
            print(m:GetSizeAsync(), (m:GetAsync("d")))
            "#,
            40,
        );
        assert_eq!(
            out,
            [
                "true\tfalse",
                "B2\t2",
                "4",
                "d,b,a,c",
                "c,a",
                "a,c",
                "A!\t6",
                "3\tnil",
            ],
            "{out:#?}"
        );
    }

    #[test]
    fn hash_maps_set_update_list_and_expire() {
        let dir = tempfile::tempdir().unwrap();
        project(dir.path(), "");
        let mut h = open(dir.path(), false);
        let out = run(
            &mut h,
            Side::Server,
            r#"
            local m = game:GetService("MemoryStoreService"):GetHashMap("Live")
            print(m:SetAsync("x", 1, 2), m:SetAsync("y", { 2 }, 60))
            print(m:GetAsync("x"), m:GetAsync("y")[1])
            print(m:UpdateAsync("x", function(v) return v + 1 end, 60))
            print(m:UpdateAsync("y", function() return nil end, 60))
            m:SetAsync("z", 3, 1)
            task.wait(1.5)
            print(m:GetAsync("z"))
            local page = m:ListItemsAsync(10):GetCurrentPage()
            print(#page, page[1].key, page[1].value)
            m:RemoveAsync("x")
            print(m:GetAsync("x"))
            print(pcall(m.SetAsync, m, "", 1))
            "#,
            200,
        );
        assert_eq!(
            &out[..7],
            ["true\ttrue", "1\t2", "2", "nil", "nil", "2\tx\t2", "nil"],
            "{out:#?}"
        );
        assert!(out[7].starts_with("false\t[RE9403] "), "{out:#?}");
    }
}
