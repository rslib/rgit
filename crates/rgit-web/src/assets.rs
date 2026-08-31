//! Static CSS and JS for the viewer, inlined into the page head/foot. A
//! mono-forward "instrument" design: JetBrains Mono for structure, a teal
//! accent kept out of the diff palette, magit-style foldable sections.

pub const CSS: &str = r#"
:root{
  --bg:#f6f7f8; --panel:#ffffff; --sunk:#eef0f2; --code:#f3f5f6; --ink:#16191d; --dim:#59616b; --faint:#98a0aa;
  --line:#e4e7ea; --line2:#d3d8dd; --acc:#0e8d7c; --acc-ink:#0a6e61; --acc-sb:#dcf1ee; --acc-line:#8fd6cc;
  --br:#2f6ab0; --add:#137a37; --add-bg:#e4f2e7; --del:#c0362c; --del-bg:#f6e5e3; --pend:#9a6b12;
  --kb:#eef0f2; --kbb:#d3d8dd; --on-acc:#ffffff;
  --mono:"JetBrains Mono",ui-monospace,SFMono-Regular,Menlo,monospace;
  --sans:"Hanken Grotesk",system-ui,-apple-system,sans-serif;
  --sh:0 1px 0 rgba(16,19,23,.03);
}
@media (prefers-color-scheme:dark){:root:not([data-theme="light"]){
  --bg:#0d0f12; --panel:#14171b; --sunk:#0a0c0f; --code:#0f1216; --ink:#e7eaec; --dim:#9aa2ad; --faint:#5c6570;
  --line:#212630; --line2:#2e3540; --acc:#54d6c4; --acc-ink:#7ee3d5; --acc-sb:#0f2b28; --acc-line:#1f4d47;
  --br:#6ea6d6; --add:#3fb950; --add-bg:#0f2416; --del:#f0665d; --del-bg:#2a1514; --pend:#d6a13a;
  --kb:#20222c; --kbb:#333542; --on-acc:#08110f; --sh:none;
}}
:root[data-theme="dark"]{
  --bg:#0d0f12; --panel:#14171b; --sunk:#0a0c0f; --code:#0f1216; --ink:#e7eaec; --dim:#9aa2ad; --faint:#5c6570;
  --line:#212630; --line2:#2e3540; --acc:#54d6c4; --acc-ink:#7ee3d5; --acc-sb:#0f2b28; --acc-line:#1f4d47;
  --br:#6ea6d6; --add:#3fb950; --add-bg:#0f2416; --del:#f0665d; --del-bg:#2a1514; --pend:#d6a13a;
  --kb:#20222c; --kbb:#333542; --on-acc:#08110f; --sh:none;
}
*{box-sizing:border-box;}
body{margin:0; padding-bottom:40px; background:var(--bg); color:var(--ink);
  font-family:var(--sans); font-size:14px; line-height:1.5; -webkit-font-smoothing:antialiased;}
.m{font-family:var(--mono);}
a{color:inherit; text-decoration:none;} a:hover{color:var(--acc-ink);}

header{position:sticky; top:0; z-index:30; display:flex; align-items:center; gap:16px;
  height:52px; padding:0 18px; background:var(--panel); border-bottom:1px solid var(--line);}
.logo{font-family:var(--mono); font-weight:700; font-size:15px; letter-spacing:-.01em; white-space:nowrap;}
.logo b{color:var(--acc-ink);}
.path{font-family:var(--mono); font-size:12.5px; color:var(--dim); white-space:nowrap;}
.path a:hover{color:var(--ink);} .path .m{color:var(--ink); font-weight:500;}
header .sp{flex:1;}
.ib{border:1px solid var(--line2); background:var(--panel); color:var(--dim); width:28px; height:28px;
  border-radius:6px; cursor:pointer; font-size:13px; display:grid; place-items:center;}
.ib:hover{color:var(--acc-ink); border-color:var(--acc-line);}

nav.tabs{display:flex; align-items:center; gap:2px; padding:0 18px; background:var(--panel);
  border-bottom:1px solid var(--line); position:sticky; top:52px; z-index:20;}
nav.tabs > a{font-family:var(--mono); font-size:12.5px; color:var(--dim); padding:10px 12px 9px; border-bottom:2px solid transparent;
  display:inline-flex; gap:6px; align-items:baseline; white-space:nowrap;}
nav.tabs > a:hover{color:var(--ink);}
nav.tabs > a .k{font-size:10px; color:var(--faint);}
nav.tabs > a.on{color:var(--ink); border-bottom-color:var(--acc);} nav.tabs > a.on .k{color:var(--acc-ink);}
nav.tabs .tabsep{width:1px; height:16px; background:var(--line2); margin:0 10px 0 4px; align-self:center;}
.refsw{position:relative; align-self:center; margin-right:2px;}
.refbtn{display:inline-flex; align-items:center; gap:7px; height:26px; padding:0 9px; font-family:var(--mono); font-size:11.5px; color:var(--dim); background:var(--sunk); border:1px solid var(--line2); border-radius:6px; cursor:pointer;}
.refbtn:hover{border-color:var(--acc-line); color:var(--ink);}
.refbtn .rbi{color:var(--faint); font-size:12px;}
.refbtn .rbl{color:var(--ink); max-width:150px; overflow:hidden; text-overflow:ellipsis; white-space:nowrap;}
.refbtn .rbc{color:var(--faint); font-size:9px;}
.refpop{position:absolute; top:32px; left:0; width:264px; z-index:45; background:var(--panel); border:1px solid var(--line2); border-radius:9px; box-shadow:0 10px 34px rgba(0,0,0,.2); display:none; overflow:hidden;}
.refsw.open .refpop{display:block;}
.rpf{padding:8px; border-bottom:1px solid var(--line);}
.rpf input{width:100%; font-family:var(--mono); font-size:12px; padding:5px 9px; border:1px solid var(--line2); border-radius:6px; background:var(--sunk); color:var(--ink); outline:none;}
.rpf input:focus{border-color:var(--acc-line); background:var(--panel);}
.rpk{display:flex; gap:6px; padding:8px; border-bottom:1px solid var(--line);}
.rpk-pill{display:inline-flex; align-items:center; gap:5px; font-family:var(--mono); font-size:11px; padding:3px 9px; border:1px solid var(--line2); border-radius:999px; background:transparent; color:var(--dim); cursor:pointer;}
.rpk-pill:hover{color:var(--ink); border-color:var(--acc-line);}
.rpk-pill.on{color:var(--acc-ink); border-color:var(--acc-line); background:var(--sunk);}
.rpk-n{font-size:10px; color:var(--faint);}
.rpk-pill.on .rpk-n{color:var(--acc-ink);}
.rpl{max-height:340px; overflow-y:auto; padding:6px;}
.rpg{font-family:var(--mono); font-size:10px; text-transform:uppercase; letter-spacing:.08em; color:var(--faint); padding:7px 8px 3px;}
.refitem{display:block; padding:5px 9px; border-radius:6px; font-family:var(--mono); font-size:12px; color:var(--ink); white-space:nowrap; overflow:hidden; text-overflow:ellipsis;}
.refitem:hover,.refitem.sel{background:var(--sunk);}
.refitem.on{color:var(--acc-ink);}
.refitem.on::after{content:"\2713"; float:right; color:var(--acc-ink);}

main{max-width:1080px; margin:0 auto; padding:16px 18px 28px;}
main.shell{max-width:1220px; display:grid; grid-template-columns:1fr 262px; gap:20px; align-items:start;}
.idxhead{display:flex; align-items:center; gap:12px; margin:2px 2px 18px;}
.idxhead h1{font-size:17px; margin:0; font-weight:600; letter-spacing:-.01em;}
.idxcount{font-family:var(--mono); font-size:12px; color:var(--faint);}
.repogrid{display:grid; grid-template-columns:repeat(auto-fill,minmax(280px,1fr)); gap:12px;}
.repocard{display:block; border:1px solid var(--line); border-radius:10px; background:var(--panel); box-shadow:var(--sh); padding:13px 15px; color:var(--ink);}
.repocard:hover{border-color:var(--acc-line);}
.rctop{display:flex; align-items:center; gap:8px;}
.rcicon{font-size:13px;} .rcname{font-family:var(--mono); font-weight:500; font-size:13.5px; color:var(--acc-ink);}
.rcvis{margin-left:auto; font-family:var(--mono); font-size:10px; color:var(--faint); border:1px solid var(--line2); border-radius:20px; padding:1px 8px;}
.rcdesc{margin:8px 0 0; font-size:12.5px; color:var(--dim); line-height:1.45; display:-webkit-box; -webkit-line-clamp:2; -webkit-box-orient:vertical; overflow:hidden; min-height:34px;}
.rcstats{display:flex; flex-wrap:wrap; align-items:center; gap:5px 14px; margin-top:11px; font-family:var(--mono); font-size:11px; color:var(--dim);}
.rcstats .rclang{display:inline-flex; align-items:center; gap:5px;} .rclang .langdot{width:9px; height:9px; border-radius:2px; display:inline-block;}
.rclast{display:flex; align-items:baseline; gap:8px; margin-top:10px; padding-top:9px; border-top:1px solid var(--line); font-family:var(--mono); font-size:11px; color:var(--faint);}
.rclast .rchash{color:var(--acc-ink);}
.rclast .rcmsg{color:var(--dim); overflow:hidden; text-overflow:ellipsis; white-space:nowrap;}
.rclast .rcwhen{margin-left:auto; white-space:nowrap;}
.idxhead .sp{flex:1;}
.repofilter{font-family:var(--mono); font-size:12px; padding:0 11px; height:30px; border-radius:7px; border:1px solid var(--line2); background:var(--sunk); color:var(--ink); min-width:200px;}
.repofilter:focus{outline:none; border-color:var(--acc-line); background:var(--panel);}
.langrow{display:flex; align-items:center; gap:8px; font-size:12.5px; padding:2px 0;}
.langrow .langdot{width:9px; height:9px; border-radius:2px; display:inline-block;}
.langrow .langl{color:var(--ink);} .langrow .langn{margin-left:auto; font-family:var(--mono); font-size:11px; color:var(--faint);}
.feed a{display:block; padding:8px 0; border-bottom:1px solid var(--line); color:var(--ink);}
.feed a:last-child{border-bottom:0; padding-bottom:0;} .feed a:first-child{padding-top:0;}
.feed .fr{display:flex; align-items:baseline; gap:8px;}
.feed .frn{font-family:var(--mono); font-size:11px; color:var(--acc-ink);}
.feed .fhh{font-family:var(--mono); font-size:10.5px; color:var(--faint);}
.feed .ft{margin-left:auto; font-family:var(--mono); font-size:10.5px; color:var(--faint);}
.feed .fs{font-size:12px; color:var(--dim); margin-top:2px; overflow:hidden; text-overflow:ellipsis; white-space:nowrap;}
.feed a:hover .fs{color:var(--ink);}
.content{min-width:0;}
.side{position:sticky; top:96px; display:flex; flex-direction:column; gap:14px;}
.card{border:1px solid var(--line); border-radius:10px; background:var(--panel); box-shadow:var(--sh);}
.card .ch{font-family:var(--mono); font-size:11px; letter-spacing:.06em; text-transform:uppercase; color:var(--faint); padding:11px 14px 0;}
.card .cb{padding:11px 14px 13px;}
.side .desc{font-size:12.5px; color:var(--dim); margin:0; line-height:1.5;}
.card .kvrow{display:flex; align-items:baseline; justify-content:space-between; gap:8px; font-size:12px; padding:3px 0;}
.card .kvrow .k{color:var(--dim);} .card .kvrow .v{font-family:var(--mono); color:var(--ink); text-align:right; word-break:break-all;}
.stat{display:flex; justify-content:space-between; align-items:baseline; font-size:13px; padding:3px 0;}
.stat .l{color:var(--dim);} .stat .v{font-family:var(--mono); color:var(--ink);}
.miniclone{display:flex; align-items:center; gap:7px; background:var(--code); border:1px solid var(--line); border-radius:8px; padding:6px 8px;}
.miniclone code{font-family:var(--mono); font-size:10.5px; flex:1; overflow-x:auto; white-space:nowrap; color:var(--dim);}
.miniclone button{border:0; background:transparent; color:var(--dim); cursor:pointer; font-size:13px;} .miniclone button:hover{color:var(--acc);}
.dl{display:inline-block; font-family:var(--mono); font-size:12px; color:var(--br);} .dl:hover{color:var(--acc);}
.rel{display:flex; align-items:baseline; gap:8px;} .rel .rt{font-family:var(--mono); color:var(--pend); font-weight:600;} .rel .rw{color:var(--faint); font-size:11px; margin-left:auto;}
.relmsg{margin:6px 0 0; font-size:12px; color:var(--dim); line-height:1.4;}
.avatars{display:flex; flex-wrap:wrap; gap:6px;}
.av{position:relative; width:26px; height:26px; border-radius:50%; display:grid; place-items:center; font-family:var(--mono); font-size:10.5px; font-weight:600; color:#fff; cursor:pointer; text-decoration:none;}
.av:hover{outline:2px solid var(--acc); outline-offset:1px;}
.av:hover::after{content:attr(data-tip); position:absolute; bottom:calc(100% + 7px); right:0; left:auto; white-space:nowrap;
  background:var(--ink); color:var(--bg); font-family:var(--sans); font-size:11px; padding:3px 8px; border-radius:6px; z-index:9; pointer-events:none; box-shadow:var(--sh);}
.filterbar{display:flex; align-items:center; gap:8px; padding:11px 14px; border-bottom:1px solid var(--line); font-size:12.5px; color:var(--dim);}
.filterbar .fauthor{font-family:var(--mono); color:var(--acc); font-weight:600;}
.filterbar .fclear{margin-left:auto; font-size:11.5px; color:var(--br);} .filterbar .fclear:hover{color:var(--acc);}

/* foldable sections */
.sec > .h{cursor:pointer; user-select:none;}
.sec > .h .caret{color:var(--faint); font-size:10px; width:11px; display:inline-block; transition:transform .12s;}
.sec.fold > .h .caret{transform:rotate(-90deg);}
.sec.fold > .body{display:none;}
.sec > .h .aside{font-family:var(--mono); font-size:11.5px; color:var(--dim);}
a.aside{color:var(--dim);} a.aside:hover{color:var(--acc);}

/* header clone popover + MCP chip */
.btn{display:inline-flex; align-items:center; gap:6px; font-family:var(--mono); font-size:11px; height:28px; padding:0 10px; border-radius:6px; border:1px solid var(--line2); background:var(--panel); color:var(--dim); cursor:pointer;}
.btn.pri{background:var(--acc); border-color:var(--acc); color:var(--on-acc); font-weight:500;} .btn.pri:hover{filter:brightness(1.06);}
.cw{position:relative;}
.cpop{position:absolute; right:0; top:calc(100% + 8px); width:330px; z-index:40; background:var(--panel); border:1px solid var(--line2); border-radius:10px; box-shadow:0 10px 34px rgba(0,0,0,.2); padding:13px; display:none;}
.cpop.open{display:block;}
.cpop .ch2{font-family:var(--mono); font-size:10.5px; letter-spacing:.07em; text-transform:uppercase; color:var(--faint); margin-bottom:9px;}
.copyrow{display:flex; align-items:center; gap:8px; background:var(--sunk); border:1px solid var(--line); border-radius:7px; padding:6px 9px; margin-bottom:7px;}
.copyrow .rn{font-family:var(--mono); font-size:10.5px; color:var(--pend);}
.copyrow code{font-family:var(--mono); font-size:11px; flex:1; overflow-x:auto; white-space:nowrap; color:var(--dim);}
.copyrow button{border:0; background:transparent; color:var(--dim); cursor:pointer; font-size:13px;} .copyrow button:hover{color:var(--acc-ink);}
.cpop .hint{margin:0; font-size:11px; color:var(--faint);} .cpop .dl2{margin-top:8px;}
.chip-mcp{position:relative; display:inline-flex; align-items:center; gap:6px; font-family:var(--mono); font-size:11px; height:28px; padding:0 10px; border-radius:6px; background:var(--acc-sb); color:var(--acc-ink); border:1px solid var(--acc-line); cursor:pointer; text-decoration:none;}
.chip-mcp:hover{border-color:var(--acc); color:var(--acc-ink);}
.chip-mcp .dot{width:6px; height:6px; border-radius:50%; background:var(--acc);}
.chip-tip{position:absolute; top:calc(100% + 8px); right:0; width:250px; padding:11px 13px; background:var(--panel); border:1px solid var(--line2); border-radius:9px; box-shadow:var(--sh); font-family:var(--sans); font-size:11.5px; line-height:1.5; letter-spacing:0; text-transform:none; color:var(--dim); text-align:left; white-space:normal; display:flex; flex-direction:column; gap:6px; opacity:0; visibility:hidden; transform:translateY(-4px); transition:opacity .12s ease, transform .12s ease; pointer-events:none; z-index:60;}
.chip-mcp:hover .chip-tip{opacity:1; visibility:visible; transform:translateY(0);}
.chip-tip b{color:var(--ink); font-weight:600; font-size:12px;}
.chip-tip .tiprow{display:flex; align-items:center; gap:7px;}
.chip-tip .tipk{font-family:var(--mono); font-size:9.5px; text-transform:uppercase; letter-spacing:.05em; color:var(--faint); width:32px; flex:none;}
.chip-tip code{font-family:var(--mono); font-size:11px; color:var(--acc-ink); background:var(--bg); border:1px solid var(--line2); border-radius:5px; padding:1px 6px; word-break:break-all; min-width:0;}
.chip-tip .tipro{font-family:var(--mono); font-size:9.5px; color:var(--faint); margin-left:auto; flex:none;}
.chip-tip .tipmore{color:var(--acc-ink); font-size:11px; margin-top:1px;}
.toollist{display:flex; flex-direction:column; gap:8px;}
.toolrow{border:1px solid var(--line); border-radius:10px; background:var(--panel); padding:10px 13px;}
.toolrow:hover{border-color:var(--acc-line);}
.toolhd{display:flex; align-items:center; flex-wrap:wrap; gap:6px; margin-bottom:4px;}
.tooln{font-family:var(--mono); font-size:13px; font-weight:600; color:var(--acc-ink);}
.targ{font-family:var(--mono); font-size:10.5px; padding:1px 6px; border-radius:5px; background:var(--sunk); color:var(--dim); border:1px solid var(--line2);}
.targ.req{color:var(--acc-ink); border-color:var(--acc-line); background:var(--acc-sb);}
.twrite{font-family:var(--mono); font-size:10px; letter-spacing:.04em; text-transform:uppercase; padding:1px 6px; border-radius:5px; color:var(--del); border:1px solid color-mix(in srgb,var(--del) 40%,transparent); background:color-mix(in srgb,var(--del) 10%,transparent);}
.toolrow.writes .tooln{color:var(--ink);}
.toold{margin:0 !important; color:var(--dim); font-size:12.5px !important; line-height:1.5;}

/* scoped code search */
.hsearch{position:relative; flex:1; margin:0;}
.hsearch .sbox{display:flex; align-items:center; height:32px; border:1px solid var(--line2); border-radius:7px; background:var(--sunk); overflow:hidden;}
.hsearch .sbox:focus-within{border-color:var(--acc-line); background:var(--panel);}
.hsearch .scope{display:flex; align-items:center; gap:6px; font-family:var(--mono); font-size:11.5px; color:var(--acc-ink); background:var(--acc-sb); height:100%; padding:0 9px; border-right:1px solid var(--acc-line); white-space:nowrap;}
.hsearch input{flex:1; border:0; outline:0; background:transparent; color:var(--ink); font-family:var(--mono); font-size:12.5px; padding:0 10px; min-width:40px;}
.hsearch input::placeholder{color:var(--faint);}
.hsearch .slash{font-family:var(--mono); font-size:11px; color:var(--faint); border:1px solid var(--line2); border-radius:4px; padding:1px 6px; margin-right:7px;}
/* search autocomplete dropdown */
.aclist{position:absolute; top:38px; left:0; right:0; background:var(--panel); border:1px solid var(--line2); border-radius:8px; box-shadow:0 10px 34px rgba(0,0,0,.2); padding:6px; z-index:40; display:none; max-height:340px; overflow-y:auto;}
.hsearch.acopen .aclist{display:block;}
.acitem{display:flex; align-items:baseline; gap:10px; padding:5px 9px; border-radius:6px; cursor:pointer;}
.acitem.on{background:var(--sunk);}
.acitem .ack{font-family:var(--mono); font-size:12px; color:var(--acc-ink); white-space:nowrap;}
.acitem .acd{font-size:12px; color:var(--faint); margin-left:auto; overflow:hidden; text-overflow:ellipsis; white-space:nowrap;}

.srhead{padding:12px 14px; border-bottom:1px solid var(--line); font-family:var(--mono); font-size:12.5px; color:var(--dim);} .srhead .srq{color:var(--acc-ink); font-weight:500;}
.grepo{display:flex; align-items:baseline; gap:8px; padding:10px 14px 6px; border-top:1px solid var(--line); background:var(--sunk);}
.grepo:first-of-type{border-top:0;} .grepo .grepon{font-size:12.5px; color:var(--acc-ink); font-weight:600;}
/* text/meaning toggle + semantic hits */
.modetabs{display:inline-flex; border:1px solid var(--line2); border-radius:7px; overflow:hidden; margin-bottom:14px;}
.modetabs a{font-family:var(--mono); font-size:11.5px; padding:4px 13px; color:var(--dim); border-right:1px solid var(--line2);}
.modetabs a:last-child{border-right:0;} .modetabs a:hover{color:var(--acc-ink);}
.modetabs a.on{background:var(--acc-sb); color:var(--acc-ink);}
.semhit{display:block; border:1px solid var(--line); border-radius:9px; background:var(--panel); box-shadow:var(--sh); padding:9px 12px; margin-bottom:9px;}
.semhit:hover{border-color:var(--acc-line);}
.semtop{display:flex; align-items:baseline; gap:10px;}
.semtop .sempath{font-size:12.5px; color:var(--acc-ink); font-weight:500; overflow:hidden; text-overflow:ellipsis; white-space:nowrap;}
.semtop .semloc{font-size:11px; color:var(--faint);}
.semtop .semscore{margin-left:auto; font-size:11px; color:var(--faint);}
.semprev{margin:7px 0 0; font-family:var(--mono); font-size:12px; color:var(--dim); white-space:pre; overflow-x:auto;}
.srfile{display:flex; align-items:baseline; gap:8px; padding:10px 14px 5px; border-top:1px solid var(--line);}
.srfile:first-of-type{border-top:0;}
.srfile .srpath{font-family:var(--mono); font-size:12px; color:var(--acc-ink); font-weight:500;} .srfile .srn{font-family:var(--mono); font-size:11px; color:var(--faint);}
.srline{display:flex; gap:12px; padding:2px 14px; align-items:baseline;} .srline:hover{background:var(--sunk);}
.srline .srlno{color:var(--faint); font-size:11.5px; min-width:44px; text-align:right; font-variant-numeric:tabular-nums;}
.srline .srtext{color:var(--dim); font-size:12.5px; white-space:pre; overflow-x:auto; overflow-y:hidden;}
.srtext mark{background:var(--acc-sb); color:var(--acc-ink); border-radius:2px; padding:0 1px;}
.refsel{font-family:var(--mono); font-size:11.5px; color:var(--dim); background:var(--sunk); border:1px solid var(--line2); border-radius:6px; padding:0 8px; height:32px; max-width:180px; cursor:pointer;}
.refsel:hover{border-color:var(--acc-line); color:var(--ink);}
.tnode.cur{background:var(--acc-sb); border-radius:6px;}
.tnode.cur a{color:var(--acc-ink); font-weight:600;}

/* language bar */
.langbar{display:flex; height:7px; border-radius:4px; overflow:hidden; margin:11px 0 8px;}
.langbar .lb{display:block; height:100%;}
.langkey{display:flex; flex-wrap:wrap; gap:5px 12px; font-size:11px; color:var(--dim);}
.langkey span{display:inline-flex; align-items:center; gap:5px;}
.langkey .lb{width:8px; height:8px; border-radius:2px; display:inline-block;}
.lb.l0{background:var(--acc);} .lb.l1{background:var(--br);} .lb.l2{background:var(--add);} .lb.l3{background:var(--pend);} .lb.l4{background:var(--line2);}

/* lanes + agent panel */
.change{grid-template-columns:22px 1fr; padding-left:14px;}
.change .stc{font-family:var(--mono); font-weight:600; text-align:center;}
.change .stc.g{color:var(--add);} .change .stc.y{color:var(--pend);} .change .stc.r{color:var(--del);}
.change .stp{color:var(--ink); overflow:hidden; text-overflow:ellipsis; white-space:nowrap;} .change .stp a{color:var(--ink);} .change .stp a:hover{color:var(--acc);}
.change .storig{color:var(--faint);}
.lane{padding:9px 14px; border-bottom:1px solid var(--line);} .lane:last-child{border-bottom:0;}
.lane .top{display:flex; align-items:baseline; gap:9px;}
.lane .nm{font-family:var(--mono); color:var(--ink); font-weight:600;}
.lane .bn{font-family:var(--mono); font-size:11.5px; color:var(--br);}
.lane .onp{font-family:var(--mono); font-size:11px; color:var(--pend);}
.lane .files{margin-top:4px; color:var(--dim); font-size:12px;} .lane .files .fp{font-family:var(--mono);} .lane .files .cm{color:var(--faint);}
.agent{padding:14px;} .agent p{margin:0 0 11px; color:var(--dim); font-size:12.5px; line-height:1.5;} .agent code{font-family:var(--mono); color:var(--acc);}
.agent .ep{font-family:var(--mono); font-size:12px; display:flex; flex-wrap:wrap; justify-content:flex-start; gap:4px 8px; color:var(--ink); background:var(--bg); border:1px solid var(--line2); border-radius:8px; padding:8px 11px; word-break:break-all;}
.agent .ep .k{color:var(--acc);}
.agent .ep code{word-break:break-all; min-width:0;}
.mcpopt{margin:0 0 14px; padding:12px; border:1px solid var(--line2); border-radius:10px; background:var(--bg);}
.mcphd{display:flex; align-items:center; gap:8px; font-weight:600; font-size:12.5px; margin-bottom:6px;}
.mcphd .mcpn{display:inline-flex; align-items:center; justify-content:center; width:18px; height:18px; border-radius:5px; background:var(--acc-sb); color:var(--acc-ink); border:1px solid var(--acc-line); font-family:var(--mono); font-size:11px; font-weight:700;}
.mcpsub{margin:8px 0 6px !important; color:var(--dim); font-size:12px !important; line-height:1.5;}
.mcpcmd{margin:0; background:var(--bg); border:1px solid var(--line2); border-radius:8px; padding:8px 11px;}
.mcpcmd code{font-family:var(--mono); font-size:12px; color:var(--ink) !important; white-space:pre-wrap; word-break:break-word;}
.prbadge{font-family:var(--mono); font-size:11px; color:var(--pend); border:1px solid color-mix(in srgb,var(--pend) 40%,transparent); border-radius:6px; padding:1px 7px; margin-left:7px;}
.prbadge:hover{color:var(--acc);}
.prbadge .ci.p{color:var(--add);} .prbadge .ci.f{color:var(--del);} .prbadge .ci.r{color:var(--pend);}

/* tree view: left file explorer (3-pane with the sidebar) */
.treegrid{display:grid; grid-template-columns:206px 1fr; gap:16px; align-items:start;}
.treemain{min-width:0;}
.explorer{border:1px solid var(--line); border-radius:10px; background:var(--panel); box-shadow:var(--sh); overflow:hidden; position:sticky; top:96px; max-height:calc(100vh - 130px); display:flex; flex-direction:column;}
.explorer .eh{padding:9px 11px; border-bottom:1px solid var(--line); font-size:10.5px; font-weight:700; letter-spacing:.11em; text-transform:uppercase; color:var(--dim);}
.etree{overflow-y:auto; padding:6px; font-family:var(--mono); font-size:12px;}
.tnode{display:flex; align-items:center; gap:5px; padding:3px 7px; border-radius:6px; white-space:nowrap;}
.tnode:hover{background:color-mix(in srgb,var(--acc) 7%,transparent);}
.tnode a{color:var(--ink); overflow:hidden; text-overflow:ellipsis;} .tnode a:hover{color:var(--acc);}
.tnode .caret2{color:var(--faint); font-size:9px; width:9px; text-align:center;}
.tnode.file .caret2{visibility:hidden;}
.tnode .eic{font-size:11px;} .tnode.dir .eic{color:var(--br);} .tnode.file .eic{color:var(--faint);}
.tkids{padding-left:12px;}
@media (max-width:900px){ .treegrid{grid-template-columns:1fr;} .explorer{position:static; max-height:320px;} }
@media (max-width:900px){ main.shell{grid-template-columns:1fr;} .side{position:static; flex-direction:row; flex-wrap:wrap;} .side .card{flex:1; min-width:220px;} }
.sec{border:1px solid var(--line); border-radius:10px; background:var(--panel); box-shadow:var(--sh); margin-bottom:14px; overflow:hidden;}
.sec > .h{display:flex; align-items:center; gap:9px; padding:10px 14px;}
.sec > .h .title{font-family:var(--mono); font-size:12.5px; font-weight:500; letter-spacing:.02em; color:var(--ink);}
.sec > .h .count{font-family:var(--mono); font-size:11px; color:var(--faint);}
.sec > .h .sp{flex:1;}
.sec > .h .aside{font-family:var(--mono); font-size:11px; color:var(--faint);}
.sec > .h + .body{border-top:1px solid var(--line);}

.row{display:grid; align-items:baseline; gap:11px; padding:6px 14px 6px 22px; border-bottom:1px solid var(--line);
  position:relative;}
.row:last-child{border-bottom:0;}
.row.kbcur{background:var(--sunk);}
.row.kbcur::before{content:"\25B8"; position:absolute; left:8px; top:6px; color:var(--acc); font-size:10px;}
/* Keyboard cursor (j/k): a soft tint and a thin left accent rail, never a hard
   box. Block points (cards, search hits) and inline fallback links share it. */
[data-point].kbcur:not(.row){background:var(--sunk); box-shadow:inset 2px 0 0 var(--acc); border-radius:4px;}
tr[data-point].kbcur{background:var(--sunk); box-shadow:inset 2px 0 0 var(--acc);}
/* Inline fallback links (sidebar, explorer): an accent underline reads cleaner
   than a box around a run of text. */
a.kbcur:not([data-point]){text-decoration:underline; text-decoration-color:var(--acc); text-decoration-thickness:2px; text-underline-offset:3px; color:var(--acc-ink);}
/* Fallback buttons / copy controls (e.g. the sidebar Clone card): a ring, since
   they are standalone controls rather than rows or inline links. */
button.kbcur:not([data-point]), [data-copy].kbcur:not(a):not([data-point]), [data-copy-text].kbcur:not(a):not([data-point]){box-shadow:0 0 0 2px var(--acc); border-radius:6px; color:var(--acc-ink);}
/* Focused pane (h/l): a continuous left rail, drawn as an overlay so inner cards
   cannot chop it into segments the way an inset shadow gets occluded. Only the
   static main/list panes get position:relative; the explorer and info sidebar
   are already position:sticky (a relative override there would cancel sticky and
   drop them 96px via their top:96px), and sticky already anchors the overlay. */
.content, .treemain, .diffmain{position:relative;}
[data-pane].panefocus::before{content:""; position:absolute; left:0; top:0; bottom:0; width:2px; background:var(--acc); border-radius:2px; z-index:3; pointer-events:none;}
.row .hash{color:var(--acc-ink); font-weight:500;} .row .hash a{color:var(--acc-ink);} .row .hash a:hover{text-decoration:underline;}
.row .subj{color:var(--ink); min-width:0; overflow:hidden; text-overflow:ellipsis; white-space:nowrap;}
.row .meta{color:var(--faint); font-size:11.5px; white-space:nowrap;} .row .meta .a{color:var(--dim);}
.commit-row{grid-template-columns:16px 62px 1fr auto;}
.node{font-family:var(--mono); font-size:10px; color:var(--faint);}
.commit-row.head .node{color:var(--acc);}
.ref{font-family:var(--mono); font-size:10.5px; margin-left:7px; border:1px solid var(--acc-line); border-radius:4px; padding:0 5px; color:var(--acc-ink);}
.ref.local{color:var(--acc-ink); border-color:var(--acc-line);} .ref.remote{color:var(--dim); border-color:var(--line2);} .ref.tag{color:var(--pend); border-color:color-mix(in srgb,var(--pend) 40%,transparent);}
.up{color:var(--pend); margin-left:6px;}

.headline{display:flex; flex-wrap:wrap; align-items:baseline; gap:8px 18px; padding:13px 15px;}
.headline .b{font-family:var(--mono); color:var(--ink); font-weight:600;}
.headline .kv{font-size:12px; color:var(--dim);}
.pill{display:inline-flex; align-items:center; gap:6px; font-family:var(--mono); font-size:11.5px;
  padding:2px 9px; border-radius:999px; border:1px solid var(--line2);}
.pill.ok{color:var(--add); border-color:color-mix(in srgb,var(--add) 40%,transparent); background:var(--add-bg);}
.pill.br{color:var(--br); border-color:color-mix(in srgb,var(--br) 35%,transparent);}
.ah{color:var(--add);} .bh{color:var(--del);}

.crumb{padding:11px 14px; border-bottom:1px solid var(--line); font-family:var(--mono); font-size:12.5px;}
.crumb a{color:var(--br);} .crumb .s{color:var(--faint); margin:0 5px;}
.tree-row{grid-template-columns:20px 1.2fr 2fr auto; padding-left:14px;}
.tree-row .nm{font-family:var(--mono); color:var(--ink);}
.tree-row .lc{color:var(--dim); font-size:12px; overflow:hidden; text-overflow:ellipsis; white-space:nowrap;}
.tree-row .lc a{color:var(--dim);} .tree-row .lc a:hover{color:var(--acc);}
.ic{font-size:12.5px;} .ic.d{color:var(--br);} .ic.f{color:var(--faint);}
.sz{color:var(--faint); font-size:11.5px; font-family:var(--mono);}

.filehead{display:flex; align-items:center; gap:12px; padding:10px 14px; border-bottom:1px solid var(--line);
  font-family:var(--mono); font-size:12px; color:var(--dim);}
.filehead .p{color:var(--ink);} .filehead .sp{flex:1;} .filehead .fsz{color:var(--faint); font-size:11.5px;}
.filehead > a{color:var(--dim); font-size:11.5px;} .filehead > a:hover{color:var(--acc-ink);}
.seg{display:flex; border:1px solid var(--line2); border-radius:6px; overflow:hidden;}
.seg a{font-family:var(--mono); font-size:11.5px; padding:3px 11px; color:var(--dim); border-right:1px solid var(--line2);}
.seg a:last-child{border-right:0;} .seg a:hover{color:var(--acc-ink);}
.seg a.on{background:var(--acc-sb); color:var(--acc-ink);}
.code{display:grid; grid-template-columns:auto 1fr; font-family:var(--mono); font-size:12.5px;
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
.markdown-body code{font-family:var(--mono); font-size:.88em; background:var(--code); padding:.15em .4em; border-radius:5px;}
.markdown-body pre{background:var(--code); border:1px solid var(--line); border-radius:8px; padding:12px 14px; overflow-x:auto;}
.markdown-body pre code{background:none; padding:0; font-size:12.5px;}
.markdown-body ul,.markdown-body ol{margin:0 0 1em; padding-left:1.6em;} .markdown-body li{margin:.2em 0;}
.markdown-body blockquote{margin:0 0 1em; padding:.2em 1em; border-left:3px solid var(--line2); color:var(--dim);}
.markdown-body table{border-collapse:collapse; margin:0 0 1em; display:block; overflow-x:auto;}
.markdown-body th,.markdown-body td{border:1px solid var(--line2); padding:6px 12px;} .markdown-body th{background:var(--code);}
.markdown-body img{max-width:100%;}
.markdown-body hr{border:0; border-top:1px solid var(--line); margin:1.6em 0;}

/* commit diff: changed-files sidebar + list/single mode */
.difflayout{display:grid; grid-template-columns:236px 1fr; gap:16px; align-items:start;}
.difflayout .diffmain{min-width:0;}
.difffiles{position:sticky; top:96px; max-height:calc(100vh - 130px); display:flex; flex-direction:column;
  border:1px solid var(--line); border-radius:10px; background:var(--panel); box-shadow:var(--sh); overflow:hidden;}
.dfh{display:flex; align-items:center; gap:8px; padding:10px 12px; border-bottom:1px solid var(--line);}
.dfh .dft{font-family:var(--mono); font-size:11px; letter-spacing:.06em; text-transform:uppercase; color:var(--faint);}
.dfh .dfn{font-family:var(--mono); font-size:11px; color:var(--faint); margin-left:auto;}
.dfmode{display:flex; gap:4px; padding:8px 10px; border-bottom:1px solid var(--line);}
.dfm{font-family:var(--mono); font-size:11px; color:var(--dim); padding:3px 11px; border:1px solid var(--line2); border-radius:6px; cursor:pointer;}
.dfm.on{color:var(--acc-ink); background:var(--acc-sb); border-color:var(--acc-line);}
.dflist{overflow-y:auto; padding:6px;}
.dfitem{display:flex; align-items:baseline; gap:8px; padding:5px 8px; border-radius:6px; cursor:pointer; color:var(--ink);}
.dfitem:hover{background:var(--sunk);}
.dfitem.on{background:var(--acc-sb);}
.dfitem .dfp{font-family:var(--mono); font-size:11.5px; overflow:hidden; text-overflow:ellipsis; white-space:nowrap;}
.dfitem.on .dfp{color:var(--acc-ink);}
.dfitem .dfs{margin-left:auto; font-family:var(--mono); font-size:10.5px; white-space:nowrap;}
.dfitem .dfs b.p{color:var(--add);} .dfitem .dfs b.m{color:var(--del);}
.diffloading{padding:20px 14px; color:var(--faint); font-family:var(--mono); font-size:12px;}
@media (max-width:900px){ .difflayout{grid-template-columns:1fr;} .difffiles{position:static; max-height:280px;} }

.cmeta{padding:14px 15px; border-bottom:1px solid var(--line);}
.cmeta .subj{font-size:15px; font-weight:600; margin:0 0 7px;}
.cmeta .full{font-family:var(--mono); font-size:12px; color:var(--acc); word-break:break-all;}
.cmeta .by{color:var(--dim); font-size:12px; margin-top:5px;} .cmeta .by b{color:var(--ink); font-weight:600;}
.cactions{margin-top:11px;}
.cbtn{display:inline-flex; align-items:center; gap:7px; font-family:var(--mono); font-size:11.5px; color:var(--dim);
  padding:5px 11px; border:1px solid var(--line2); border-radius:7px; background:var(--panel);}
.cbtn:hover{color:var(--acc-ink); border-color:var(--acc-line);}
.filediff{border:1px solid var(--line); border-radius:10px; overflow:hidden; margin:12px 0; background:var(--panel); box-shadow:var(--sh);}
.filediff .fh{display:flex; justify-content:space-between; align-items:center; padding:8px 13px; border-bottom:1px solid var(--line);
  font-family:var(--mono); font-size:12px; color:var(--dim);} .filediff .fh .p{color:var(--ink);}
.diffstat{font-family:var(--mono); font-size:11.5px; color:var(--dim);}
.diffstat b.p{color:var(--add);} .diffstat b.m{color:var(--del);}
.hunk pre{margin:0; padding:6px 0; overflow-x:auto; font-family:var(--mono); font-size:12.5px; line-height:1.55; background:var(--code);}
.ln{display:block; padding:0 13px; white-space:pre;}
.ln.h{color:var(--dim); background:color-mix(in srgb,var(--br) 8%,transparent);}
.ln.a{background:var(--add-bg); color:var(--add);} .ln.d{background:var(--del-bg); color:var(--del);}

.tbl{width:100%; border-collapse:collapse;}
.tbl th{text-align:left; font-size:10.5px; letter-spacing:.06em; text-transform:uppercase; color:var(--faint); font-weight:600; padding:9px 14px; border-bottom:1px solid var(--line);}
.tbl td{padding:9px 14px; border-bottom:1px solid var(--line); font-size:12.5px; vertical-align:baseline;}
.tbl tr:last-child td{border-bottom:0;} .tbl tr:hover td{background:color-mix(in srgb,var(--acc) 6%,transparent);}
.tbl .nm{font-family:var(--mono); color:var(--ink);}
.tbl .gl{margin-right:7px; color:var(--br);} .tbl .gl.t{color:var(--pend);}
.tbl .badge{font-family:var(--mono); font-size:10.5px; color:var(--acc); border:1px solid var(--line2); border-radius:5px; padding:1px 6px;}

.rel-row{grid-template-columns:minmax(180px,260px) 1fr auto;}
.rel-tag{display:inline-flex; align-items:center; gap:6px; color:var(--acc-ink);}
.rel-tag .gl.t{color:var(--pend);} .rel-tag a{color:var(--acc-ink);} .rel-tag a:hover{text-decoration:underline;}
.rel-latest{font-family:var(--mono); font-size:10px; text-transform:uppercase; letter-spacing:.06em; color:var(--acc); border:1px solid var(--acc-line); border-radius:999px; padding:1px 7px;}
.rel-msg{color:var(--dim); min-width:0; overflow:hidden; text-overflow:ellipsis; white-space:nowrap;}

.blameline{display:grid; grid-template-columns:158px 46px 1fr; align-items:baseline;
  font-family:var(--mono); font-size:12.5px; line-height:1.6;}
.blameline .who{color:var(--dim); padding:0 10px; border-right:1px solid var(--line); background:var(--panel);
  overflow:hidden; text-overflow:ellipsis; white-space:nowrap;}
.blameline .who .bh{color:var(--acc-ink);} .blameline .who .ba{color:var(--dim);}
.blameline .who a:hover{color:var(--acc-ink);}
.blameline .no{text-align:right; color:var(--faint); padding-right:12px; border-right:1px solid var(--line);
  background:var(--panel); font-variant-numeric:tabular-nums;}
.blameline .bt{padding:0 12px; white-space:pre; overflow-x:auto;}

.pager{display:flex; justify-content:space-between; align-items:center; padding:11px 14px;}
.btn{display:inline-flex; align-items:center; gap:7px; font-size:12px; padding:6px 11px; border-radius:8px;
  border:1px solid var(--line2); background:var(--panel); color:var(--ink); cursor:pointer;}
.btn:hover{border-color:var(--acc); color:var(--acc);} .btn.off{opacity:.4; pointer-events:none;}

.statusline{position:fixed; left:0; right:0; bottom:0; height:30px; display:flex; align-items:center; gap:14px;
  padding:0 16px; background:var(--panel); color:var(--dim); border-top:1px solid var(--line);
  font-family:var(--mono); font-size:11.5px; z-index:40;}
.statusline .mode{background:var(--acc-sb); color:var(--acc-ink); border:1px solid var(--acc-line); padding:1px 9px; border-radius:4px; font-size:10.5px; letter-spacing:.07em;}
.statusline .ctxl{color:var(--dim); overflow:hidden; text-overflow:ellipsis; white-space:nowrap;}
.statusline .sp{flex:1;} .statusline .keys{color:var(--faint);} .statusline .keys b{color:var(--acc-ink); font-weight:500;}
kbd{font-family:var(--mono); font-size:11px; color:var(--dim); background:var(--kb);
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
.finput .pfx{font-family:var(--mono); color:var(--acc); font-size:15px;}
.finput input{flex:1; font:inherit; font-size:14px; border:0; background:transparent; color:var(--ink); outline:none;}
#find-list{max-height:46vh; overflow-y:auto; padding:6px;}
.fitem{display:block; padding:7px 11px; border-radius:7px; font-family:var(--mono); font-size:12.5px;
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
.pitem .phint{margin-left:auto; font-size:11px; color:var(--faint); font-family:var(--mono);}
"#;

pub const JS: &str = r#"
var root=document.documentElement;
var BASE=(document.body&&document.body.getAttribute('data-base'))||'';
// Fill the MCP endpoint URL in the header chip tooltip from this page's origin.
Array.prototype.forEach.call(document.querySelectorAll('.mcpchipurl'),function(el){el.textContent=location.origin+'/mcp';});
try{var s=localStorage.getItem('rgit-theme'); if(s) root.setAttribute('data-theme',s);}catch(e){}
var tb=document.getElementById('theme');
if(tb) tb.addEventListener('click',function(){
  var n=root.getAttribute('data-theme')==='dark'?'light':'dark';
  root.setAttribute('data-theme',n); try{localStorage.setItem('rgit-theme',n);}catch(e){}
});
function slice(n){return Array.prototype.slice.call(n);}
// j/k operate within the focused pane when one is set (h/l), else the whole
// page. A pane with no explicit [data-point] rows falls back to its links, so
// every pane - sidebar included - is navigable.
// Leaf panes only: a pane that contains another pane (e.g. the content column
// wrapping the file explorer + code) is a container, not a focus target.
function panes(){return slice(document.querySelectorAll('[data-pane]')).filter(function(p){return !p.querySelector('[data-pane]');});}
function apane(){return document.querySelector('[data-pane].panefocus');}
function scopeEl(){return apane()||document;}
function rows(){var s=scopeEl(); var p=s.querySelectorAll('[data-point]'); if(p.length)return slice(p);
  if(s!==document)return slice(s.querySelectorAll('a[href],button,[data-copy],[data-copy-text]')); return [];}
function cur(){return document.querySelector('.kbcur');}
function select(el){if(!el)return; var c=cur(); if(c)c.classList.remove('kbcur'); el.classList.add('kbcur'); el.scrollIntoView({block:'nearest'});}
// Geometry-aware cursor: move to the nearest navigable in a compass direction,
// so a card grid steps down/up by row (j/k) and across by column (h/l), while a
// plain list still just goes next/previous. The perpendicular offset is weighted
// so "down" prefers the same column rather than drifting sideways.
function centers(){return rows().map(function(el){var r=el.getBoundingClientRect(); return {el:el, x:r.left+r.width/2, y:r.top+r.height/2};});}
function nearest(dir){var list=centers(); if(!list.length)return null;
  var c=cur(); var from=null; for(var i=0;i<list.length;i++){if(list[i].el===c){from=list[i];break;}}
  if(!from)return list[0].el;
  var best=null, bestScore=Infinity;
  for(var j=0;j<list.length;j++){var p=list[j]; if(p.el===c)continue; var dx=p.x-from.x, dy=p.y-from.y, ok, along, perp;
    if(dir==='down'){ok=dy>1; along=dy; perp=Math.abs(dx);}
    else if(dir==='up'){ok=dy<-1; along=-dy; perp=Math.abs(dx);}
    else if(dir==='right'){ok=dx>1; along=dx; perp=Math.abs(dy);}
    else {ok=dx<-1; along=-dx; perp=Math.abs(dy);}
    if(!ok)continue; var score=along+perp*2; if(score<bestScore){bestScore=score; best=p.el;}}
  return best;}
function move(d){var t=nearest(d>0?'down':'up'); if(t){select(t); return true;}
  if(!cur()){var r=rows(); if(r.length){select(r[0]); return true;}} return false;}
// h/l: step across a grid row first; only cross to the neighbouring pane when
// there is no navigable item that way (a plain list, or the row edge).
function horiz(d){if(cur()){var t=nearest(d>0?'right':'left'); if(t){select(t); return true;}} return focusPane(d);}
function focusPane(d){var ps=panes(); if(ps.length<2)return false;
  // Start from the pane the cursor is actually in, so the first cross moves to a
  // real neighbour rather than snapping to pane 0 (or the last pane).
  var f=apane(); if(!f){var c=cur(); f=c&&c.closest&&c.closest('[data-pane]');}
  var i=ps.indexOf(f); if(i<0)i=(d>0?-1:0); i=(i+d+ps.length)%ps.length;
  ps.forEach(function(p){p.classList.remove('panefocus');});
  var c=cur(); if(c)c.classList.remove('kbcur');
  ps[i].classList.add('panefocus');
  var r=rows(); if(r.length){select(r[0]); return true;}
  ps[i].scrollIntoView({block:'nearest'}); return true;}
var toastEl=document.getElementById('toast');
function toast(msg){ if(!toastEl)return; toastEl.textContent=msg; toastEl.classList.add('show'); setTimeout(function(){toastEl.classList.remove('show');},1100); }
document.addEventListener('click',function(e){
  var cp=e.target.closest('[data-copy]'); if(cp){ e.preventDefault(); var el=document.querySelector(cp.getAttribute('data-copy')); if(el){ try{navigator.clipboard.writeText(el.textContent);}catch(x){} toast('copied'); } return; }
  var ct=e.target.closest('[data-copy-text]'); if(ct){ e.preventDefault(); try{navigator.clipboard.writeText(ct.getAttribute('data-copy-text'));}catch(x){} toast('copied'); return; }
  var h=e.target.closest('.h'); if(h && h.parentElement.classList.contains('sec') && !e.target.closest('a')){ h.parentElement.classList.toggle('fold'); }
});
// Commit diff: the changed-files sidebar loads each file's diff on demand from
// the backend (/commit/<rev>/diff/<path>), so a commit touching many files never
// renders every hunk up front. "one" shows a single file; "list" appends all,
// progressively. Fetched fragments are cached per file.
var difflayout=document.querySelector('.difflayout');
if(difflayout){
  var dfitems=Array.prototype.slice.call(difflayout.querySelectorAll('.dfitem'));
  var dfmodes=Array.prototype.slice.call(difflayout.querySelectorAll('.dfm'));
  var diffbox=document.getElementById('diffbox');
  var dfmode='one', dfcache={};
  function dfFetch(url){ return dfcache[url]?Promise.resolve(dfcache[url]):fetch(url).then(function(r){return r.text();}).then(function(h){dfcache[url]=h;return h;}); }
  function dfMark(item){ dfitems.forEach(function(a){ a.classList.toggle('on',a===item); }); }
  function dfWrap(item,h){ return '<div class="dfwrap" data-file="'+item.getAttribute('data-file')+'">'+h+'</div>'; }
  function dfOne(item){ dfMark(item); diffbox.innerHTML='<div class="diffloading">'+(diffbox.getAttribute('data-loading')||'loading')+'</div>';
    dfFetch(item.getAttribute('data-diff')).then(function(h){ diffbox.innerHTML=dfWrap(item,h); }).catch(function(){ diffbox.innerHTML='<div class="diffloading">failed to load diff</div>'; }); }
  function dfList(){ var url=diffbox.getAttribute('data-diffs'); if(!url){ return; }
    diffbox.innerHTML='<div class="diffloading">'+(diffbox.getAttribute('data-loading')||'loading')+'</div>';
    fetch(url).then(function(r){return r.text();}).then(function(h){ diffbox.innerHTML=h; }).catch(function(){ diffbox.innerHTML='<div class="diffloading">failed to load diffs</div>'; }); }
  dfitems.forEach(function(item){ item.addEventListener('click',function(e){ e.preventDefault();
    if(dfmode==='one'){ dfOne(item); }
    else { dfMark(item); var t=diffbox.querySelector('.dfwrap[data-file="'+item.getAttribute('data-file')+'"]'); if(t)t.scrollIntoView({block:'start'}); }
  }); });
  dfmodes.forEach(function(b){ b.addEventListener('click',function(e){ e.preventDefault();
    dfmode=b.getAttribute('data-mode'); dfmodes.forEach(function(x){ x.classList.toggle('on',x===b); });
    if(dfmode==='one'){ dfOne(difflayout.querySelector('.dfitem.on')||dfitems[0]); } else { dfList(); }
  }); });
  if(dfitems[0]) dfOne(dfitems[0]);
}

// PR/CI badges: fetch lazily so page load never waits on the forge.
var prb=document.getElementById('pr-badges');
if(prb){ var pu=prb.getAttribute('data-prs'); if(pu){ fetch(pu).then(function(r){return r.text();}).then(function(h){ prb.innerHTML=h; }).catch(function(){}); } }

// Index repo filter.
var repoFilter=document.getElementById('repo-filter');
if(repoFilter){ repoFilter.addEventListener('input',function(){ var q=repoFilter.value.toLowerCase();
  Array.prototype.forEach.call(document.querySelectorAll('.repocard'),function(c){
    c.style.display=(c.getAttribute('data-name')||'').toLowerCase().indexOf(q)>=0?'':'none'; }); }); }

// Header search autocomplete: complete qualifiers and their values as you type.
// repo: from /api/repos, path: from the current repo's file list (single-repo
// only - too much across all repos), lang:/ext: from static lists.
var AC_LANGS=['rust','python','javascript','typescript','go','c','cpp','java','kotlin','swift','ruby','php','shell','bash','toml','yaml','json','markdown','html','css','scss','sql','lua','haskell','ocaml','zig','make','cmake','dockerfile','rst'];
var AC_EXTS=['rs','py','js','mjs','ts','tsx','jsx','go','c','h','cpp','cc','hpp','java','kt','swift','rb','php','sh','bash','toml','yaml','yml','json','md','html','css','scss','sql','lua','zig'];
var AC_QUALS=[{k:'repo:',d:'one repository'},{k:'lang:',d:'by language'},{k:'path:',d:'by path (single repo)'},{k:'ext:',d:'by extension'}];
var acRepos=null, acFiles=null;
function acLoadRepos(cb){ if(acRepos){cb(acRepos);return;} fetch('/api/repos').then(function(r){return r.json();}).then(function(j){acRepos=j||[];cb(acRepos);}).catch(function(){cb([]);}); }
function acLoadFiles(cb){ if(acFiles){cb(acFiles);return;} if(!BASE){cb([]);return;} fetch(BASE+'/files').then(function(r){return r.text();}).then(function(t){acFiles=t?t.split('\n').filter(Boolean):[];cb(acFiles);}).catch(function(){cb([]);}); }
var hsInput=null;
Array.prototype.forEach.call(document.querySelectorAll('.hsearch'),function(hs){
  var input=hs.querySelector('input'), list=hs.querySelector('.aclist');
  if(!input||!list) return;
  if(!hsInput) hsInput=input;
  var items=[], sel=-1;
  function token(){ var v=input.value, pos=input.selectionStart==null?v.length:input.selectionStart;
    var s=pos; while(s>0 && !/\s/.test(v[s-1])) s--; var e=pos; while(e<v.length && !/\s/.test(v[e])) e++;
    return {start:s, end:e, upto:v.slice(s,pos)}; }
  function paint(){ var els=list.querySelectorAll('.acitem'); Array.prototype.forEach.call(els,function(el,i){el.classList.toggle('on',i===sel);}); var on=list.querySelector('.acitem.on'); if(on)on.scrollIntoView({block:'nearest'}); }
  function render(arr){ items=arr; sel=arr.length?0:-1;
    if(!arr.length){ hs.classList.remove('acopen'); list.innerHTML=''; return; }
    var h=''; for(var i=0;i<arr.length;i++){ h+='<div class="acitem'+(i===0?' on':'')+'" data-i="'+i+'"><span class="ack">'+esc(arr[i].label)+'</span><span class="acd">'+esc(arr[i].hint||'')+'</span></div>'; }
    list.innerHTML=h; hs.classList.add('acopen'); }
  function suggest(){ var t=token(), m=t.upto.match(/^(repo|lang|path|ext):(.*)$/i);
    if(m){ var key=m[1].toLowerCase(), val=m[2].toLowerCase();
      if(key==='lang') render(AC_LANGS.filter(function(l){return l.indexOf(val)===0;}).slice(0,30).map(function(l){return {ins:'lang:'+l,label:l,hint:'language'};}));
      else if(key==='ext') render(AC_EXTS.filter(function(x){return x.indexOf(val)===0;}).slice(0,30).map(function(x){return {ins:'ext:'+x,label:x,hint:'extension'};}));
      else if(key==='repo') acLoadRepos(function(rs){ render(rs.filter(function(r){return r.toLowerCase().indexOf(val)>=0;}).slice(0,30).map(function(r){return {ins:'repo:'+r,label:r,hint:'repository'};})); });
      else if(key==='path'){ acLoadFiles(function(fs){ render(fs.filter(function(f){return f.toLowerCase().indexOf(val)>=0;}).slice(0,20).map(function(f){return {ins:'path:'+f,label:f,hint:'path'};})); }); }
      return; }
    var q=t.upto.toLowerCase();
    render(AC_QUALS.filter(function(x){return q===''||x.k.indexOf(q)===0;}).map(function(x){return {ins:x.k,label:x.k,hint:x.d};})); }
  function accept(i){ if(i<0||i>=items.length)return; var t=token(), v=input.value, ins=items[i].ins;
    var tail=ins.charAt(ins.length-1)===':'?'':' ';
    input.value=v.slice(0,t.start)+ins+tail+v.slice(t.end);
    var p=(v.slice(0,t.start)+ins+tail).length; input.setSelectionRange(p,p);
    hs.classList.remove('acopen'); input.focus(); if(tail==='') suggest(); }
  input.addEventListener('input',suggest);
  input.addEventListener('focus',suggest);
  input.addEventListener('blur',function(){ setTimeout(function(){hs.classList.remove('acopen');},150); });
  input.addEventListener('keydown',function(e){ if(!hs.classList.contains('acopen'))return;
    if(e.key==='ArrowDown'){e.preventDefault(); sel=Math.min(items.length-1,sel+1); paint();}
    else if(e.key==='ArrowUp'){e.preventDefault(); sel=Math.max(0,sel-1); paint();}
    else if(e.key==='Tab'){e.preventDefault(); accept(sel);}
    else if(e.key==='Enter'){ if(items[sel]&&items[sel].ins.charAt(items[sel].ins.length-1)===':'){ e.preventDefault(); accept(sel); } else { hs.classList.remove('acopen'); } }
    else if(e.key==='Escape'){ hs.classList.remove('acopen'); } });
  list.addEventListener('mousedown',function(e){ var it=e.target.closest('.acitem'); if(it){ e.preventDefault(); accept(+it.getAttribute('data-i')); } });
});

// Ref switcher: a searchable branch/tag dropdown (custom component, not a native
// select). Items are links, so navigation is a plain click; the input filters and
// the arrow keys move a highlight.
var refsw=document.getElementById('refsw');
if(refsw){
  var refbtn=document.getElementById('refbtn'), refInput=document.getElementById('ref-input'), refList=document.getElementById('ref-list');
  var refKinds=document.getElementById('ref-kinds'), refKind='all';
  var refVisible=function(){return Array.prototype.slice.call(refList.querySelectorAll('.refitem')).filter(function(e){return e.style.display!=='none';});};
  var refFilter=function(){
    var q=refInput.value.toLowerCase();
    Array.prototype.forEach.call(refList.querySelectorAll('.refitem'),function(e){
      var okText=(e.getAttribute('data-ref')||'').toLowerCase().indexOf(q)>=0;
      var okKind=refKind==='all'||e.getAttribute('data-kind')===refKind;
      e.style.display=(okText&&okKind)?'':'none'; e.classList.remove('sel');
    });
    Array.prototype.forEach.call(refList.querySelectorAll('.rpg'),function(g){
      var n=g.nextElementSibling, any=false;
      while(n&&!n.classList.contains('rpg')){ if(n.classList.contains('refitem')&&n.style.display!=='none'){any=true;break;} n=n.nextElementSibling; }
      g.style.display=any?'':'none';
    });
    var vis=refVisible(); if(vis[0])vis[0].classList.add('sel');
  };
  var refOpen=function(){ refsw.classList.add('open'); refInput.value=''; refFilter(); setTimeout(function(){refInput.focus();},10); };
  var refClose=function(){ refsw.classList.remove('open'); };
  refbtn.addEventListener('click',function(e){ e.stopPropagation(); refsw.classList.contains('open')?refClose():refOpen(); });
  document.addEventListener('click',function(e){ if(!refsw.contains(e.target)) refClose(); });
  refInput.addEventListener('input',refFilter);
  if(refKinds){ refKinds.addEventListener('click',function(e){ var p=e.target.closest('.rpk-pill'); if(!p)return;
    refKind=p.getAttribute('data-kind');
    Array.prototype.forEach.call(refKinds.querySelectorAll('.rpk-pill'),function(b){b.classList.toggle('on',b===p);});
    refFilter(); refInput.focus(); }); }
  refInput.addEventListener('keydown',function(e){
    var vis=refVisible(), cur=refList.querySelector('.refitem.sel'), i=cur?vis.indexOf(cur):-1;
    if(e.key==='ArrowDown'){ e.preventDefault(); if(cur)cur.classList.remove('sel'); var nx=vis[Math.min(vis.length-1,i+1)]; if(nx){nx.classList.add('sel'); nx.scrollIntoView({block:'nearest'});} }
    else if(e.key==='ArrowUp'){ e.preventDefault(); if(cur)cur.classList.remove('sel'); var pv=vis[Math.max(0,i-1)]; if(pv){pv.classList.add('sel'); pv.scrollIntoView({block:'nearest'});} }
    else if(e.key==='Enter'){ e.preventDefault(); var go=cur||vis[0]; if(go)location.href=go.href; }
    else if(e.key==='Escape'){ e.preventDefault(); refClose(); refbtn.focus(); }
  });
}

var cloneBtn=document.getElementById('cloneBtn'), cpop=document.getElementById('cpop');
if(cloneBtn&&cpop){
  cloneBtn.addEventListener('click',function(e){ e.stopPropagation(); cpop.classList.toggle('open'); });
  document.addEventListener('click',function(e){ if(!cpop.contains(e.target)&&!cloneBtn.contains(e.target)) cpop.classList.remove('open'); });
}
var scrim=document.getElementById('scrim'), wk=document.getElementById('whichkey');
var finder=document.getElementById('finder'), findInput=document.getElementById('find-input'), findList=document.getElementById('find-list');
var palette=document.getElementById('palette'), palInput=document.getElementById('pal-input'), palList=document.getElementById('pal-list');
function closeAll(){ if(wk)wk.classList.remove('open'); if(finder)finder.classList.remove('open'); if(palette)palette.classList.remove('open'); if(scrim)scrim.classList.remove('open'); var cp=document.getElementById('cpop'); if(cp)cp.classList.remove('open'); }
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
  if(e.key==='/'){if(hsInput){e.preventDefault();hsInput.focus();}return;}
  if(e.key==='?'){e.preventDefault();toggleWk();return;}
  if(e.key===':'){e.preventDefault();openPalette();return;}
  if(e.key==='t'){e.preventDefault();openFinder();return;}
  if(e.key==='c'){var cb=document.getElementById('cloneBtn'), cp=document.getElementById('cpop');
    if(cb&&cp){ e.preventDefault(); cb.click();
      if(cp.classList.contains('open')){ var f=cp.querySelector('button,a[href]'); if(f)setTimeout(function(){f.focus();},20); } }
    return;}
  if(e.key==='y'){var pl=document.querySelector('[data-permalink]'); var url=pl?pl.getAttribute('data-permalink'):location.href;
    try{navigator.clipboard.writeText(new URL(url,location.href).href);}catch(x){} toast('permalink copied');return;}
  if(e.key==='j'){if(move(1))e.preventDefault();}
  else if(e.key==='k'){if(move(-1))e.preventDefault();}
  else if(e.key==='l'){if(horiz(1))e.preventDefault();}
  else if(e.key==='h'){if(horiz(-1))e.preventDefault();}
  else if(e.key==='Enter'){var c=cur(); if(c){var a=c.matches('a[href]')?c:(c.querySelector('a[data-go]')||c.querySelector('a[href]')); if(a){location.href=a.href;} else {c.click();}}}
  else if(e.key>='1'&&e.key<='5'){var t=document.querySelectorAll('nav.tabs > a')[+e.key-1]; if(t)location.href=t.href;}
});
"#;
