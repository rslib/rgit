//! Static CSS and JS for the viewer, inlined into the page head/foot. Ported
//! from the approved "serve" design: warm paper, IBM Plex, a mono commit graph.

pub const CSS: &str = r#"
:root{
  --bg:#f5f2eb; --panel:#fbf9f4; --code:#efeadf; --ink:#241f18; --dim:#6f6a60; --faint:#a59d8f;
  --line:#e2dccd; --line2:#cec5b2; --acc:#c25a26; --acc-sb:#f6e2d5; --br:#356390; --add:#3c8646;
  --add-bg:#e4efe1; --del:#b23d3a; --del-bg:#f3e0de; --pend:#b47f22; --kb:#efe9dd; --kbb:#d6cdb9;
  --sh:0 1px 2px rgba(36,31,24,.06),0 8px 24px rgba(36,31,24,.05);
}
@media (prefers-color-scheme:dark){:root:not([data-theme="light"]){
  --bg:#111219; --panel:#191b22; --code:#0d0e14; --ink:#e9e6df; --dim:#8f8d98; --faint:#585663;
  --line:#242630; --line2:#31333f; --acc:#e2794a; --acc-sb:#291e17; --br:#6ea6d6; --add:#79b56f;
  --add-bg:#152018; --del:#d0756f; --del-bg:#231617; --pend:#d69f45; --kb:#20222c; --kbb:#333542;
  --sh:0 1px 2px rgba(0,0,0,.3),0 10px 34px rgba(0,0,0,.4);
}}
:root[data-theme="dark"]{
  --bg:#111219; --panel:#191b22; --code:#0d0e14; --ink:#e9e6df; --dim:#8f8d98; --faint:#585663;
  --line:#242630; --line2:#31333f; --acc:#e2794a; --acc-sb:#291e17; --br:#6ea6d6; --add:#79b56f;
  --add-bg:#152018; --del:#d0756f; --del-bg:#231617; --pend:#d69f45; --kb:#20222c; --kbb:#333542;
  --sh:0 1px 2px rgba(0,0,0,.3),0 10px 34px rgba(0,0,0,.4);
}
*{box-sizing:border-box;}
body{margin:0; padding-bottom:40px; background:var(--bg); color:var(--ink);
  font-family:"IBM Plex Sans",system-ui,sans-serif; font-size:13.5px; line-height:1.5;}
.m{font-family:"IBM Plex Mono",ui-monospace,monospace;}
a{color:inherit; text-decoration:none;} a:hover{color:var(--acc);}

header{position:sticky; top:0; z-index:30; display:flex; align-items:center; gap:14px;
  padding:10px 18px; background:var(--panel); border-bottom:1px solid var(--line);}
.logo{font-family:"IBM Plex Mono",monospace; font-weight:600; font-size:16px;}
.logo b{color:var(--acc);}
.path{font-size:13px; color:var(--dim);} .path b{color:var(--acc); font-weight:600;}
header .sp{flex:1;}
.ib{border:1px solid var(--line2); background:var(--panel); color:var(--dim); width:32px; height:31px;
  border-radius:8px; cursor:pointer; font-size:14px; display:grid; place-items:center;}
.ib:hover{color:var(--acc); border-color:var(--acc);}

nav.tabs{display:flex; align-items:center; gap:3px; padding:0 18px; background:var(--panel);
  border-bottom:1px solid var(--line); position:sticky; top:52px; z-index:20; overflow-x:auto;}
nav.tabs a{font-size:12.5px; color:var(--dim); padding:9px 11px 8px; border-bottom:2px solid transparent;
  display:inline-flex; gap:6px; align-items:baseline; white-space:nowrap;}
nav.tabs a .k{font-family:"IBM Plex Mono",monospace; font-size:10px; color:var(--faint);}
nav.tabs a.on{color:var(--ink); border-bottom-color:var(--acc);} nav.tabs a.on .k{color:var(--acc);}

main{max-width:1080px; margin:0 auto; padding:16px 18px 28px;}
main.shell{max-width:1220px; display:grid; grid-template-columns:1fr 262px; gap:20px; align-items:start;}
.idxhead{display:flex; align-items:baseline; gap:10px; margin:6px 2px 16px;}
.idxhead h1{font-size:20px; margin:0; font-weight:700;}
.idxcount{font-family:"IBM Plex Mono",monospace; font-size:13px; color:var(--faint);}
.repogrid{display:grid; grid-template-columns:repeat(auto-fill,minmax(280px,1fr)); gap:14px;}
.repocard{display:block; border:1px solid var(--line); border-radius:11px; background:var(--panel); box-shadow:var(--sh); padding:14px 16px; color:var(--ink);}
.repocard:hover{border-color:var(--acc);}
.rctop{display:flex; align-items:center; gap:8px;}
.rcicon{font-size:14px;} .rcname{font-weight:600; color:var(--acc);}
.rcdesc{margin:8px 0 0; font-size:12.5px; color:var(--dim); line-height:1.45; display:-webkit-box; -webkit-line-clamp:2; -webkit-box-orient:vertical; overflow:hidden;}
.rcstats{display:flex; flex-wrap:wrap; align-items:center; gap:5px 12px; margin-top:11px; font-size:11.5px; color:var(--dim);}
.rcstats .rclang{display:inline-flex; align-items:center; gap:5px;} .rclang .langdot{width:9px; height:9px; border-radius:50%; display:inline-block;}
.rclast{display:flex; align-items:baseline; gap:8px; margin-top:9px; padding-top:9px; border-top:1px solid var(--line); font-size:11.5px; color:var(--faint);}
.rclast .rchash{color:var(--acc);}
.rclast .rcmsg{color:var(--dim); overflow:hidden; text-overflow:ellipsis; white-space:nowrap;}
.rclast .rcwhen{margin-left:auto; white-space:nowrap;}
.idxhead .sp{flex:1;}
.repofilter{font:inherit; font-size:13px; padding:6px 11px; border-radius:8px; border:1px solid var(--line2); background:var(--panel); color:var(--ink); min-width:160px;}
.repofilter:focus{outline:none; border-color:var(--acc);}
.langrow{display:flex; align-items:center; gap:8px; font-size:12.5px; padding:3px 0;}
.langrow .langl{color:var(--ink);} .langrow .langn{margin-left:auto; font-family:"IBM Plex Mono",monospace; color:var(--faint);}
.content{min-width:0;}
.side{position:sticky; top:96px; display:flex; flex-direction:column; gap:14px;}
.card{border:1px solid var(--line); border-radius:11px; background:var(--panel); box-shadow:var(--sh);}
.card .ch{font-size:10.5px; font-weight:700; letter-spacing:.12em; text-transform:uppercase; color:var(--dim); padding:11px 13px 0;}
.card .cb{padding:11px 13px 13px;}
.side .desc{font-size:12.5px; color:var(--ink); margin:0; line-height:1.5;}
.card .kvrow{display:flex; align-items:baseline; justify-content:space-between; gap:8px; font-size:12px; padding:3px 0;}
.card .kvrow .k{color:var(--dim);} .card .kvrow .v{font-family:"IBM Plex Mono",monospace; color:var(--ink); text-align:right; word-break:break-all;}
.stat{display:flex; justify-content:space-between; align-items:baseline; font-size:12.5px; padding:4px 0; border-bottom:1px solid var(--line);}
.stat:last-child{border-bottom:0;} .stat .l{color:var(--dim);} .stat .v{font-family:"IBM Plex Mono",monospace; color:var(--ink); font-weight:600;}
.miniclone{display:flex; align-items:center; gap:7px; background:var(--code); border:1px solid var(--line); border-radius:8px; padding:6px 8px;}
.miniclone code{font-family:"IBM Plex Mono",monospace; font-size:10.5px; flex:1; overflow-x:auto; white-space:nowrap; color:var(--dim);}
.miniclone button{border:0; background:transparent; color:var(--dim); cursor:pointer; font-size:13px;} .miniclone button:hover{color:var(--acc);}
.dl{display:inline-block; font-family:"IBM Plex Mono",monospace; font-size:12px; color:var(--br);} .dl:hover{color:var(--acc);}
.rel{display:flex; align-items:baseline; gap:8px;} .rel .rt{font-family:"IBM Plex Mono",monospace; color:var(--pend); font-weight:600;} .rel .rw{color:var(--faint); font-size:11px; margin-left:auto;}
.relmsg{margin:6px 0 0; font-size:12px; color:var(--dim); line-height:1.4;}
.avatars{display:flex; flex-wrap:wrap; gap:6px;}
.av{position:relative; width:26px; height:26px; border-radius:50%; display:grid; place-items:center; font-family:"IBM Plex Mono",monospace; font-size:10.5px; font-weight:600; color:#fff; cursor:pointer; text-decoration:none;}
.av:hover{outline:2px solid var(--acc); outline-offset:1px;}
.av:hover::after{content:attr(data-tip); position:absolute; bottom:calc(100% + 7px); right:0; left:auto; white-space:nowrap;
  background:var(--ink); color:var(--bg); font-family:"IBM Plex Sans",sans-serif; font-size:11px; padding:3px 8px; border-radius:6px; z-index:9; pointer-events:none; box-shadow:var(--sh);}
.filterbar{display:flex; align-items:center; gap:8px; padding:11px 14px; border-bottom:1px solid var(--line); font-size:12.5px; color:var(--dim);}
.filterbar .fauthor{font-family:"IBM Plex Mono",monospace; color:var(--acc); font-weight:600;}
.filterbar .fclear{margin-left:auto; font-size:11.5px; color:var(--br);} .filterbar .fclear:hover{color:var(--acc);}

/* foldable sections */
.sec > .h{cursor:pointer; user-select:none;}
.sec > .h .caret{color:var(--faint); font-size:10px; width:11px; display:inline-block; transition:transform .12s;}
.sec.fold > .h .caret{transform:rotate(-90deg);}
.sec.fold > .body{display:none;}
.sec > .h .aside{font-family:"IBM Plex Mono",monospace; font-size:11.5px; color:var(--dim);}
a.aside{color:var(--dim);} a.aside:hover{color:var(--acc);}

/* header clone popover + MCP chip */
.btn{display:inline-flex; align-items:center; gap:7px; font-size:12px; padding:6px 11px; border-radius:8px; border:1px solid var(--line2); background:var(--panel); color:var(--ink); cursor:pointer;}
.btn.pri{background:var(--acc); border-color:var(--acc); color:#fff;} .btn.pri:hover{filter:brightness(1.07);}
.cw{position:relative;}
.cpop{position:absolute; right:0; top:calc(100% + 8px); width:330px; z-index:40; background:var(--panel); border:1px solid var(--line2); border-radius:11px; box-shadow:var(--sh); padding:13px; display:none;}
.cpop.open{display:block;}
.cpop .ch2{font-size:10.5px; letter-spacing:.09em; text-transform:uppercase; color:var(--dim); margin-bottom:9px;}
.copyrow{display:flex; align-items:center; gap:8px; background:var(--code); border:1px solid var(--line); border-radius:8px; padding:6px 9px; margin-bottom:7px;}
.copyrow .rn{font-family:"IBM Plex Mono",monospace; font-size:10.5px; color:var(--pend);}
.copyrow code{font-family:"IBM Plex Mono",monospace; font-size:11px; flex:1; overflow-x:auto; white-space:nowrap; color:var(--dim);}
.copyrow button{border:0; background:transparent; color:var(--dim); cursor:pointer; font-size:13px;} .copyrow button:hover{color:var(--acc);}
.cpop .hint{margin:0; font-size:11px; color:var(--faint);} .cpop .dl2{margin-top:8px;}
.chip-mcp{display:inline-flex; align-items:center; gap:7px; font-family:"IBM Plex Mono",monospace; font-size:11.5px; padding:5px 10px; border-radius:8px; background:var(--acc-sb); color:var(--acc); border:1px solid color-mix(in srgb,var(--acc) 35%,transparent);}
.chip-mcp .dot{width:6px; height:6px; border-radius:50%; background:var(--acc);}
.hsearch{margin:0;} .hsearch input{font:inherit; font-size:12px; padding:6px 10px; border-radius:8px; border:1px solid var(--line2); background:var(--bg); color:var(--ink); width:180px;}
.hsearch input:focus{outline:none; border-color:var(--acc); width:230px;} .hsearch input::placeholder{color:var(--faint);}
.srhead{padding:11px 14px; border-bottom:1px solid var(--line); font-size:12.5px; color:var(--dim);} .srhead .srq{color:var(--acc); font-weight:600;}
.srfile{display:flex; align-items:baseline; gap:8px; padding:9px 14px 5px; border-top:1px solid var(--line);}
.srfile:first-of-type{border-top:0;}
.srfile .srpath{font-size:12.5px; color:var(--br); font-weight:600;} .srfile .srn{font-family:"IBM Plex Mono",monospace; font-size:11px; color:var(--faint);}
.srline{display:flex; gap:12px; padding:2px 14px; align-items:baseline;} .srline:hover{background:color-mix(in srgb,var(--acc) 6%,transparent);}
.srline .srlno{color:var(--faint); font-size:11.5px; min-width:44px; text-align:right; font-variant-numeric:tabular-nums;}
.srline .srtext{color:var(--ink); font-size:12.5px; white-space:pre; overflow-x:auto; overflow-y:hidden;}
.srtext mark{background:color-mix(in srgb,var(--acc) 30%,transparent); color:inherit; border-radius:3px;}
.refsel{font-family:"IBM Plex Mono",monospace; font-size:11.5px; color:var(--dim); background:var(--bg); border:1px solid var(--line2); border-radius:8px; padding:5px 8px; max-width:190px; cursor:pointer;}
.refsel:hover{border-color:var(--acc); color:var(--ink);}
.tnode.cur{background:color-mix(in srgb,var(--acc) 13%,transparent); border-radius:6px;}
.tnode.cur a{color:var(--acc); font-weight:600;}

/* language bar */
.langbar{display:flex; height:7px; border-radius:4px; overflow:hidden; margin:11px 0 8px;}
.langbar .lb{display:block; height:100%;}
.langkey{display:flex; flex-wrap:wrap; gap:5px 12px; font-size:11px; color:var(--dim);}
.langkey span{display:inline-flex; align-items:center; gap:5px;}
.langkey .lb{width:8px; height:8px; border-radius:2px; display:inline-block;}
.lb.l0{background:var(--acc);} .lb.l1{background:var(--br);} .lb.l2{background:var(--add);} .lb.l3{background:var(--pend);} .lb.l4{background:var(--line2);}

/* lanes + agent panel */
.change{grid-template-columns:22px 1fr; padding-left:14px;}
.change .stc{font-family:"IBM Plex Mono",monospace; font-weight:600; text-align:center;}
.change .stc.g{color:var(--add);} .change .stc.y{color:var(--pend);} .change .stc.r{color:var(--del);}
.change .stp{color:var(--ink); overflow:hidden; text-overflow:ellipsis; white-space:nowrap;} .change .stp a{color:var(--ink);} .change .stp a:hover{color:var(--acc);}
.change .storig{color:var(--faint);}
.lane{padding:9px 14px; border-bottom:1px solid var(--line);} .lane:last-child{border-bottom:0;}
.lane .top{display:flex; align-items:baseline; gap:9px;}
.lane .nm{font-family:"IBM Plex Mono",monospace; color:var(--ink); font-weight:600;}
.lane .bn{font-family:"IBM Plex Mono",monospace; font-size:11.5px; color:var(--br);}
.lane .onp{font-family:"IBM Plex Mono",monospace; font-size:11px; color:var(--pend);}
.lane .files{margin-top:4px; color:var(--dim); font-size:12px;} .lane .files .fp{font-family:"IBM Plex Mono",monospace;} .lane .files .cm{color:var(--faint);}
.agent{padding:14px;} .agent p{margin:0 0 11px; color:var(--dim); font-size:12.5px; line-height:1.5;} .agent code{font-family:"IBM Plex Mono",monospace; color:var(--acc);}
.agent .ep{font-family:"IBM Plex Mono",monospace; font-size:12px; display:flex; justify-content:space-between; gap:8px; color:var(--ink); background:var(--bg); border:1px solid var(--line2); border-radius:8px; padding:8px 11px;}
.agent .ep .k{color:var(--acc);}
.prbadge{font-family:"IBM Plex Mono",monospace; font-size:11px; color:var(--pend); border:1px solid color-mix(in srgb,var(--pend) 40%,transparent); border-radius:6px; padding:1px 7px; margin-left:7px;}
.prbadge:hover{color:var(--acc);}
.prbadge .ci.p{color:var(--add);} .prbadge .ci.f{color:var(--del);} .prbadge .ci.r{color:var(--pend);}

/* tree view: left file explorer (3-pane with the sidebar) */
.treegrid{display:grid; grid-template-columns:206px 1fr; gap:16px; align-items:start;}
.treemain{min-width:0;}
.explorer{border:1px solid var(--line); border-radius:10px; background:var(--panel); box-shadow:var(--sh); overflow:hidden; position:sticky; top:96px; max-height:calc(100vh - 130px); display:flex; flex-direction:column;}
.explorer .eh{padding:9px 11px; border-bottom:1px solid var(--line); font-size:10.5px; font-weight:700; letter-spacing:.11em; text-transform:uppercase; color:var(--dim);}
.etree{overflow-y:auto; padding:6px; font-family:"IBM Plex Mono",monospace; font-size:12px;}
.tnode{display:flex; align-items:center; gap:5px; padding:3px 7px; border-radius:6px; white-space:nowrap;}
.tnode:hover{background:color-mix(in srgb,var(--acc) 7%,transparent);}
.tnode a{color:var(--ink); overflow:hidden; text-overflow:ellipsis;} .tnode a:hover{color:var(--acc);}
.tnode .caret2{color:var(--faint); font-size:9px; width:9px; text-align:center;}
.tnode.file .caret2{visibility:hidden;}
.tnode .eic{font-size:11px;} .tnode.dir .eic{color:var(--br);} .tnode.file .eic{color:var(--faint);}
.tkids{padding-left:12px;}
@media (max-width:900px){ .treegrid{grid-template-columns:1fr;} .explorer{position:static; max-height:320px;} }
@media (max-width:900px){ main.shell{grid-template-columns:1fr;} .side{position:static; flex-direction:row; flex-wrap:wrap;} .side .card{flex:1; min-width:220px;} }
.sec{margin-bottom:14px;}
.sec > .h{display:flex; align-items:center; gap:9px; padding:5px 8px;}
.sec > .h .title{font-size:11px; font-weight:700; letter-spacing:.12em; text-transform:uppercase; color:var(--dim);}
.sec > .h .count{font-family:"IBM Plex Mono",monospace; font-size:11px; color:var(--faint);}
.sec > .h .sp{flex:1;}
.sec > .h .aside{font-family:"IBM Plex Mono",monospace; font-size:11.5px; color:var(--dim);}
.sec > .body{border:1px solid var(--line); border-radius:10px; background:var(--panel); box-shadow:var(--sh); overflow:hidden;}

.row{display:grid; align-items:baseline; gap:11px; padding:7px 13px 7px 22px; border-bottom:1px solid var(--line);
  position:relative;}
.row:last-child{border-bottom:0;}
.row.on{background:color-mix(in srgb,var(--acc) 7%,transparent);}
.row.on::before{content:"\25B8"; position:absolute; left:7px; top:7px; color:var(--acc); font-size:10px;}
.row .hash{color:var(--acc); font-weight:500;}
.row .subj{color:var(--ink); min-width:0; overflow:hidden; text-overflow:ellipsis; white-space:nowrap;}
.row .meta{color:var(--faint); font-size:11.5px; white-space:nowrap;} .row .meta .a{color:var(--dim);}
.commit-row{grid-template-columns:18px 66px 1fr auto;}
.node{font-family:"IBM Plex Mono",monospace; font-size:12px; color:var(--br);}
.commit-row.head .node{color:var(--acc);}
.ref{font-family:"IBM Plex Mono",monospace; font-size:10.5px; margin-left:7px;}
.ref.local{color:var(--add);} .ref.remote{color:var(--dim);} .ref.tag{color:var(--pend);}
.up{color:var(--add); margin-left:6px;}

.headline{display:flex; flex-wrap:wrap; align-items:baseline; gap:8px 18px; padding:13px 15px;}
.headline .b{font-family:"IBM Plex Mono",monospace; color:var(--ink); font-weight:600;}
.headline .kv{font-size:12px; color:var(--dim);}
.pill{display:inline-flex; align-items:center; gap:6px; font-family:"IBM Plex Mono",monospace; font-size:11.5px;
  padding:2px 9px; border-radius:999px; border:1px solid var(--line2);}
.pill.ok{color:var(--add); border-color:color-mix(in srgb,var(--add) 40%,transparent); background:var(--add-bg);}
.pill.br{color:var(--br); border-color:color-mix(in srgb,var(--br) 35%,transparent);}
.ah{color:var(--add);} .bh{color:var(--del);}

.crumb{padding:11px 14px; border-bottom:1px solid var(--line); font-family:"IBM Plex Mono",monospace; font-size:12.5px;}
.crumb a{color:var(--br);} .crumb .s{color:var(--faint); margin:0 5px;}
.tree-row{grid-template-columns:20px 1.2fr 2fr auto; padding-left:14px;}
.tree-row .nm{font-family:"IBM Plex Mono",monospace; color:var(--ink);}
.tree-row .lc{color:var(--dim); font-size:12px; overflow:hidden; text-overflow:ellipsis; white-space:nowrap;}
.tree-row .lc a{color:var(--dim);} .tree-row .lc a:hover{color:var(--acc);}
.ic{font-size:12.5px;} .ic.d{color:var(--br);} .ic.f{color:var(--faint);}
.sz{color:var(--faint); font-size:11.5px; font-family:"IBM Plex Mono",monospace;}

.filehead{display:flex; align-items:center; gap:12px; padding:10px 14px; border-bottom:1px solid var(--line);
  font-family:"IBM Plex Mono",monospace; font-size:12px; color:var(--dim);}
.filehead .p{color:var(--ink);} .filehead .sp{flex:1;}
.filehead .vt{color:var(--dim);} .filehead .vt.on{color:var(--acc); font-weight:600;} .filehead .vt:hover{color:var(--acc);}
.code{display:grid; grid-template-columns:auto 1fr; font-family:"IBM Plex Mono",monospace; font-size:12.5px;
  line-height:1.65; background:var(--code);}
.code .g{text-align:right; color:var(--faint); padding:10px 12px; user-select:none; border-right:1px solid var(--line);
  background:var(--panel); font-variant-numeric:tabular-nums; white-space:pre;}
.code .g a{display:block; color:var(--faint);} .code .g a:target{color:var(--acc); font-weight:600;}
.code .src{padding:10px 14px; overflow-x:auto;} .code .src pre{margin:0; white-space:pre;}
.binary{padding:22px 14px; color:var(--dim); text-align:center;}

.markdown-body{padding:22px 26px; font-size:14px; line-height:1.65; color:var(--ink); overflow-wrap:break-word;}
.markdown-body h1,.markdown-body h2,.markdown-body h3,.markdown-body h4,.markdown-body h5{font-weight:700; line-height:1.25; margin:1.3em 0 .5em;}
.markdown-body h1{font-size:1.7em; border-bottom:1px solid var(--line); padding-bottom:.3em;}
.markdown-body h2{font-size:1.4em; border-bottom:1px solid var(--line); padding-bottom:.3em;}
.markdown-body h3{font-size:1.2em;} .markdown-body h4{font-size:1.05em;}
.markdown-body p{margin:0 0 1em;}
.markdown-body a{color:var(--br);} .markdown-body a:hover{color:var(--acc); text-decoration:underline;}
.markdown-body code{font-family:"IBM Plex Mono",monospace; font-size:.88em; background:var(--code); padding:.15em .4em; border-radius:5px;}
.markdown-body pre{background:var(--code); border:1px solid var(--line); border-radius:8px; padding:12px 14px; overflow-x:auto;}
.markdown-body pre code{background:none; padding:0; font-size:12.5px;}
.markdown-body ul,.markdown-body ol{margin:0 0 1em; padding-left:1.6em;} .markdown-body li{margin:.2em 0;}
.markdown-body blockquote{margin:0 0 1em; padding:.2em 1em; border-left:3px solid var(--line2); color:var(--dim);}
.markdown-body table{border-collapse:collapse; margin:0 0 1em; display:block; overflow-x:auto;}
.markdown-body th,.markdown-body td{border:1px solid var(--line2); padding:6px 12px;} .markdown-body th{background:var(--code);}
.markdown-body img{max-width:100%;}
.markdown-body hr{border:0; border-top:1px solid var(--line); margin:1.6em 0;}

.cmeta{padding:14px 15px; border-bottom:1px solid var(--line);}
.cmeta .subj{font-size:15px; font-weight:600; margin:0 0 7px;}
.cmeta .full{font-family:"IBM Plex Mono",monospace; font-size:12px; color:var(--acc); word-break:break-all;}
.cmeta .by{color:var(--dim); font-size:12px; margin-top:5px;} .cmeta .by b{color:var(--ink); font-weight:600;}
.filediff{border:1px solid var(--line); border-radius:10px; overflow:hidden; margin:12px 0; background:var(--panel); box-shadow:var(--sh);}
.filediff .fh{display:flex; justify-content:space-between; align-items:center; padding:8px 13px; border-bottom:1px solid var(--line);
  font-family:"IBM Plex Mono",monospace; font-size:12px; color:var(--dim);} .filediff .fh .p{color:var(--ink);}
.diffstat{font-family:"IBM Plex Mono",monospace; font-size:11.5px; color:var(--dim);}
.diffstat b.p{color:var(--add);} .diffstat b.m{color:var(--del);}
.hunk pre{margin:0; padding:6px 0; overflow-x:auto; font-family:"IBM Plex Mono",monospace; font-size:12.5px; line-height:1.55; background:var(--code);}
.ln{display:block; padding:0 13px; white-space:pre;}
.ln.h{color:var(--dim); background:color-mix(in srgb,var(--br) 8%,transparent);}
.ln.a{background:var(--add-bg); color:var(--add);} .ln.d{background:var(--del-bg); color:var(--del);}

.tbl{width:100%; border-collapse:collapse;}
.tbl th{text-align:left; font-size:10.5px; letter-spacing:.06em; text-transform:uppercase; color:var(--faint); font-weight:600; padding:9px 14px; border-bottom:1px solid var(--line);}
.tbl td{padding:9px 14px; border-bottom:1px solid var(--line); font-size:12.5px; vertical-align:baseline;}
.tbl tr:last-child td{border-bottom:0;} .tbl tr:hover td{background:color-mix(in srgb,var(--acc) 6%,transparent);}
.tbl .nm{font-family:"IBM Plex Mono",monospace; color:var(--ink);}
.tbl .gl{margin-right:7px; color:var(--br);} .tbl .gl.t{color:var(--pend);}
.tbl .badge{font-family:"IBM Plex Mono",monospace; font-size:10.5px; color:var(--acc); border:1px solid var(--line2); border-radius:5px; padding:1px 6px;}

.blameline{display:grid; grid-template-columns:158px 46px 1fr; align-items:baseline;
  font-family:"IBM Plex Mono",monospace; font-size:12.5px; line-height:1.6;}
.blameline .who{color:var(--dim); padding:0 10px; border-right:1px solid var(--line); background:var(--panel);
  overflow:hidden; text-overflow:ellipsis; white-space:nowrap;} .blameline .who b{color:var(--acc);}
.blameline .no{text-align:right; color:var(--faint); padding-right:12px; border-right:1px solid var(--line);
  background:var(--panel); font-variant-numeric:tabular-nums;}
.blameline .bt{padding:0 12px; white-space:pre; overflow-x:auto;}

.pager{display:flex; justify-content:space-between; align-items:center; padding:11px 14px;}
.btn{display:inline-flex; align-items:center; gap:7px; font-size:12px; padding:6px 11px; border-radius:8px;
  border:1px solid var(--line2); background:var(--panel); color:var(--ink); cursor:pointer;}
.btn:hover{border-color:var(--acc); color:var(--acc);} .btn.off{opacity:.4; pointer-events:none;}

.statusline{position:fixed; left:0; right:0; bottom:0; height:30px; display:flex; align-items:center; gap:14px;
  padding:0 14px; background:var(--ink); color:var(--bg); font-family:"IBM Plex Mono",monospace; font-size:11.5px; z-index:40;}
.statusline .mode{background:var(--acc); color:#fff; padding:1px 8px; border-radius:4px; font-weight:600;}
.statusline .ctxl{opacity:.9; overflow:hidden; text-overflow:ellipsis; white-space:nowrap;}
.statusline .sp{flex:1;} .statusline .keys{opacity:.85;} .statusline .keys b{color:var(--acc);}
kbd{font-family:"IBM Plex Mono",monospace; font-size:11px; color:var(--dim); background:var(--kb);
  border:1px solid var(--kbb); border-radius:5px; padding:1px 5px;}

.scrim{position:fixed; inset:0; z-index:60; background:rgba(0,0,0,.28); display:none;}
.scrim.open{display:block;}
#whichkey{position:fixed; z-index:61; bottom:44px; left:50%; transform:translateX(-50%); width:min(560px,94vw);
  background:var(--panel); border:1px solid var(--line2); border-radius:12px; box-shadow:var(--sh); padding:16px 18px; display:none;}
#whichkey.open{display:block;}
#whichkey h3{margin:0 0 12px; font-size:11px; letter-spacing:.1em; text-transform:uppercase; color:var(--dim);}
.wk{display:grid; grid-template-columns:1fr 1fr; gap:7px 20px;}
.wk > div{display:flex; gap:8px; align-items:center; font-size:12.5px;} .wk .d{color:var(--dim);}
.toast{position:fixed; bottom:44px; left:50%; transform:translateX(-50%) translateY(12px); opacity:0;
  background:var(--ink); color:var(--bg); font-size:12px; padding:7px 15px; border-radius:999px; transition:.2s; z-index:70; pointer-events:none;}
.toast.show{opacity:1; transform:translateX(-50%) translateY(0);}

#finder{position:fixed; z-index:61; top:12vh; left:50%; transform:translateX(-50%); width:min(600px,94vw);
  background:var(--panel); border:1px solid var(--line2); border-radius:12px; box-shadow:var(--sh); display:none; overflow:hidden;}
#finder.open{display:block;}
.finput{display:flex; align-items:center; gap:10px; padding:12px 15px; border-bottom:1px solid var(--line);}
.finput .pfx{font-family:"IBM Plex Mono",monospace; color:var(--acc); font-size:15px;}
.finput input{flex:1; font:inherit; font-size:14px; border:0; background:transparent; color:var(--ink); outline:none;}
#find-list{max-height:46vh; overflow-y:auto; padding:6px;}
.fitem{display:block; padding:7px 11px; border-radius:7px; font-family:"IBM Plex Mono",monospace; font-size:12.5px;
  color:var(--ink); cursor:pointer; white-space:nowrap; overflow:hidden; text-overflow:ellipsis;}
.fitem.on{background:color-mix(in srgb,var(--acc) 11%,transparent);}
.fitem .d{color:var(--faint);}
.fempty{padding:14px; color:var(--faint); font-size:12.5px;}
#palette{position:fixed; z-index:61; top:12vh; left:50%; transform:translateX(-50%); width:min(560px,94vw);
  background:var(--panel); border:1px solid var(--line2); border-radius:12px; box-shadow:var(--sh); display:none; overflow:hidden;}
#palette.open{display:block;}
#pal-list{max-height:46vh; overflow-y:auto; padding:6px;}
.pitem{display:flex; align-items:center; gap:11px; padding:8px 11px; border-radius:8px; font-size:13px; color:var(--ink); cursor:pointer;}
.pitem.on{background:color-mix(in srgb,var(--acc) 11%,transparent);}
.pitem .pic{color:var(--br); width:16px; text-align:center; font-size:12px;}
.pitem .phint{margin-left:auto; font-size:11px; color:var(--faint); font-family:"IBM Plex Mono",monospace;}
"#;

pub const JS: &str = r#"
var root=document.documentElement;
var BASE=(document.body&&document.body.getAttribute('data-base'))||'';
try{var s=localStorage.getItem('rgit-theme'); if(s) root.setAttribute('data-theme',s);}catch(e){}
var tb=document.getElementById('theme');
if(tb) tb.addEventListener('click',function(){
  var n=root.getAttribute('data-theme')==='dark'?'light':'dark';
  root.setAttribute('data-theme',n); try{localStorage.setItem('rgit-theme',n);}catch(e){}
});
function rows(){return Array.prototype.slice.call(document.querySelectorAll('[data-point]'));}
function cur(){return document.querySelector('[data-point].on');}
function move(d){var r=rows(); if(!r.length)return; var c=cur(); var i=c?r.indexOf(c):-1;
  i=Math.max(0,Math.min(r.length-1,i+d)); if(c)c.classList.remove('on'); r[i].classList.add('on'); r[i].scrollIntoView({block:'nearest'});}
var toastEl=document.getElementById('toast');
function toast(msg){ if(!toastEl)return; toastEl.textContent=msg; toastEl.classList.add('show'); setTimeout(function(){toastEl.classList.remove('show');},1100); }
document.addEventListener('click',function(e){
  var cp=e.target.closest('[data-copy]'); if(cp){ e.preventDefault(); var el=document.querySelector(cp.getAttribute('data-copy')); if(el){ try{navigator.clipboard.writeText(el.textContent);}catch(x){} toast('copied'); } return; }
  var ct=e.target.closest('[data-copy-text]'); if(ct){ e.preventDefault(); try{navigator.clipboard.writeText(ct.getAttribute('data-copy-text'));}catch(x){} toast('copied'); return; }
  var h=e.target.closest('.h'); if(h && h.parentElement.classList.contains('sec') && !e.target.closest('a')){ h.parentElement.classList.toggle('fold'); }
});
// PR/CI badges: fetch lazily so page load never waits on the forge.
var prb=document.getElementById('pr-badges');
if(prb){ var pu=prb.getAttribute('data-prs'); if(pu){ fetch(pu).then(function(r){return r.text();}).then(function(h){ prb.innerHTML=h; }).catch(function(){}); } }

// Index repo filter.
var repoFilter=document.getElementById('repo-filter');
if(repoFilter){ repoFilter.addEventListener('input',function(){ var q=repoFilter.value.toLowerCase();
  Array.prototype.forEach.call(document.querySelectorAll('.repocard'),function(c){
    c.style.display=(c.getAttribute('data-name')||'').toLowerCase().indexOf(q)>=0?'':'none'; }); }); }

// Ref switcher: browse the selected branch/tag's tree.
var refsel=document.getElementById('refsel');
if(refsel){ refsel.addEventListener('change',function(){ var v=refsel.value, b=refsel.getAttribute('data-base')||'';
  location.href=b+'/tree'+(v==='HEAD'?'':'?ref='+encodeURIComponent(v)); }); }

var cloneBtn=document.getElementById('cloneBtn'), cpop=document.getElementById('cpop');
if(cloneBtn&&cpop){
  cloneBtn.addEventListener('click',function(e){ e.stopPropagation(); cpop.classList.toggle('open'); });
  document.addEventListener('click',function(e){ if(!cpop.contains(e.target)&&!cloneBtn.contains(e.target)) cpop.classList.remove('open'); });
}
var scrim=document.getElementById('scrim'), wk=document.getElementById('whichkey');
var finder=document.getElementById('finder'), findInput=document.getElementById('find-input'), findList=document.getElementById('find-list');
var palette=document.getElementById('palette'), palInput=document.getElementById('pal-input'), palList=document.getElementById('pal-list');
function closeAll(){ if(wk)wk.classList.remove('open'); if(finder)finder.classList.remove('open'); if(palette)palette.classList.remove('open'); if(scrim)scrim.classList.remove('open'); }
if(scrim) scrim.addEventListener('click',closeAll);
function toggleWk(){ if(!wk)return; var was=wk.classList.contains('open'); closeAll(); if(!was){ wk.classList.add('open'); if(scrim)scrim.classList.add('open'); } }

// Fuzzy file finder (t): fetch the file list once, subsequence-filter as you type.
var FILES=null, findSel=0, findShown=[];
function fuzzy(q,list){
  if(!q) return list.slice(0,60);
  q=q.toLowerCase(); var out=[];
  for(var i=0;i<list.length;i++){ var p=list[i].toLowerCase(), qi=0;
    for(var j=0;j<p.length&&qi<q.length;j++){ if(p.charCodeAt(j)===q.charCodeAt(qi))qi++; }
    if(qi===q.length) out.push(list[i]);
  }
  out.sort(function(a,b){return a.length-b.length;});
  return out.slice(0,60);
}
function esc(s){ return s.replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;'); }
function renderFind(){
  if(!finder) return;
  findShown=fuzzy(findInput.value, FILES||[]);
  if(!findShown.length){ findList.innerHTML='<div class="fempty">no files</div>'; return; }
  if(findSel>=findShown.length) findSel=findShown.length-1;
  if(findSel<0) findSel=0;
  var h='';
  for(var i=0;i<findShown.length;i++){ var p=findShown[i], k=p.lastIndexOf('/');
    var dir=k>=0?esc(p.slice(0,k+1)):'', base=k>=0?esc(p.slice(k+1)):esc(p);
    h+='<a class="fitem'+(i===findSel?' on':'')+'" href="'+BASE+'/blob/'+encodeURI(p)+'"><span class="d">'+dir+'</span>'+base+'</a>';
  }
  findList.innerHTML=h;
  var on=findList.querySelector('.fitem.on'); if(on)on.scrollIntoView({block:'nearest'});
}
function openFinder(){
  if(!finder) return;
  closeAll(); finder.classList.add('open'); if(scrim)scrim.classList.add('open');
  findInput.value=''; findSel=0;
  if(FILES){ renderFind(); }
  else { findList.innerHTML='<div class="fempty">loading...</div>';
    fetch(BASE+'/files').then(function(r){return r.text();}).then(function(t){ FILES=t?t.split('\n').filter(Boolean):[]; renderFind(); }); }
  setTimeout(function(){findInput.focus();},10);
}
if(findInput){
  findInput.addEventListener('input',function(){ findSel=0; renderFind(); });
  findInput.addEventListener('keydown',function(e){
    if(e.key==='ArrowDown'){e.preventDefault(); findSel++; renderFind();}
    else if(e.key==='ArrowUp'){e.preventDefault(); findSel--; renderFind();}
    else if(e.key==='Enter'){e.preventDefault(); if(findShown[findSel])location.href=BASE+'/blob/'+encodeURI(findShown[findSel]);}
    else if(e.key==='Escape'){e.preventDefault(); closeAll();}
  });
}

// Load-more: append the next page's rows without a full navigation.
var more=document.getElementById('more');
if(more) more.addEventListener('click',function(e){
  e.preventDefault();
  var next=+more.getAttribute('data-next');
  var au=more.getAttribute('data-author')||''; var aq=au?('&author='+encodeURIComponent(au)):'';
  var rf=more.getAttribute('data-ref')||''; var rq=(rf&&rf!=='HEAD')?('&ref='+encodeURIComponent(rf)):'';
  more.textContent='loading...';
  fetch(BASE+'/log?partial=1&offset='+next+aq+rq).then(function(r){return r.text();}).then(function(html){
    var box=document.getElementById('log-rows');
    box.insertAdjacentHTML('beforeend',html);
    var added=(html.match(/commit-row/g)||[]).length;
    if(added<50){ more.replaceWith(document.createTextNode('')); }
    else { more.setAttribute('data-next',next+50); more.setAttribute('href',BASE+'/log?offset='+(next+50)+aq+rq); more.textContent='load more \u2193'; }
  }).catch(function(){ more.textContent='load more \u2193'; });
});

// Command palette (:): filter the static command list, arrow-select, Enter runs.
var palSel=0;
function palItems(){ return Array.prototype.slice.call(palList.querySelectorAll('.pitem')).filter(function(el){return el.style.display!=='none';}); }
function palRender(){
  var q=palInput.value.toLowerCase();
  Array.prototype.forEach.call(palList.querySelectorAll('.pitem'),function(el){ el.style.display=el.textContent.toLowerCase().indexOf(q)>=0?'':'none'; el.classList.remove('on'); });
  var vis=palItems(); if(palSel>=vis.length)palSel=vis.length-1; if(palSel<0)palSel=0; if(vis[palSel])vis[palSel].classList.add('on');
  var on=palList.querySelector('.pitem.on'); if(on)on.scrollIntoView({block:'nearest'});
}
function openPalette(){ if(!palette)return; closeAll(); palette.classList.add('open'); if(scrim)scrim.classList.add('open'); palInput.value=''; palSel=0; palRender(); setTimeout(function(){palInput.focus();},10); }
function palRun(el){ if(!el)return;
  if(el.getAttribute('data-act')==='finder'){ openFinder(); return; }
  if(el.hasAttribute('data-copy-text')){ try{navigator.clipboard.writeText(el.getAttribute('data-copy-text'));}catch(x){} toast('copied'); closeAll(); return; }
  if(el.getAttribute('href')){ location.href=el.getAttribute('href'); }
}
if(palInput){
  palInput.addEventListener('input',function(){ palSel=0; palRender(); });
  palInput.addEventListener('keydown',function(e){
    if(e.key==='ArrowDown'){e.preventDefault(); palSel++; palRender();}
    else if(e.key==='ArrowUp'){e.preventDefault(); palSel--; palRender();}
    else if(e.key==='Enter'){e.preventDefault(); palRun(palItems()[palSel]);}
    else if(e.key==='Escape'){e.preventDefault(); closeAll();}
  });
  palList.addEventListener('click',function(e){ var it=e.target.closest('.pitem'); if(it&&(it.getAttribute('data-act')||it.hasAttribute('data-copy-text'))){ e.preventDefault(); palRun(it); } });
}

document.addEventListener('keydown',function(e){
  if(/^(INPUT|TEXTAREA)$/.test(document.activeElement.tagName))return;
  if(e.key==='Escape'){closeAll();return;}
  if(e.key==='?'){e.preventDefault();toggleWk();return;}
  if(e.key===':'){e.preventDefault();openPalette();return;}
  if(e.key==='t'){e.preventDefault();openFinder();return;}
  if(e.key==='y'){var pl=document.querySelector('[data-permalink]'); var url=pl?pl.getAttribute('data-permalink'):location.href;
    try{navigator.clipboard.writeText(new URL(url,location.href).href);}catch(x){} toast('permalink copied');return;}
  if(e.key==='j'){e.preventDefault();move(1);}
  else if(e.key==='k'){e.preventDefault();move(-1);}
  else if(e.key==='Enter'){var c=cur(); var a=c&&c.querySelector('a[data-go]'); if(a)location.href=a.href;}
  else if(e.key>='1'&&e.key<='6'){var t=document.querySelectorAll('nav.tabs a')[+e.key-1]; if(t)location.href=t.href;}
});
"#;
