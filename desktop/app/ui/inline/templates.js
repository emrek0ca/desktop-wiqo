(function(){
  /* WiqoTemplates.mount(root, config?): ready-made small tools drawn from a config (wiqo.data by default).
     kinds: checklist, counter, timer, calculator, converter, quiz, table, chart. Everything runs on the phone. */
  var css='.wq{max-width:520px;margin:0 auto}.wq h2{font-size:17px;margin:0 0 10px}.wq .row{display:flex;gap:8px;align-items:center;flex-wrap:wrap}.wq .big{font-size:44px;font-weight:600;font-variant-numeric:tabular-nums;text-align:center;margin:8px 0}'
   +'.wq .item{display:flex;gap:10px;align-items:center;padding:10px 4px;border-bottom:1px solid var(--border)}.wq .item.done span{text-decoration:line-through;color:var(--muted)}'
   +'.wq .bar{height:6px;border-radius:3px;background:var(--surface);overflow:hidden;margin:8px 0}.wq .bar i{display:block;height:100%;background:var(--accent);transition:width .2s}'
   +'.wq .grid{display:grid;grid-template-columns:repeat(4,1fr);gap:8px}.wq .grid button{min-height:52px;font-size:20px}.wq .screen{font-size:32px;text-align:right;padding:8px 4px;min-height:56px;overflow-x:auto;white-space:nowrap;font-variant-numeric:tabular-nums}'
   +'.wq .opt{display:block;width:100%;text-align:left;margin:6px 0}.wq .ok{background:#30d158!important;color:#fff!important}.wq .bad{background:#ff453a!important;color:#fff!important}.wq .muted{color:var(--muted);font-size:13px}'
   +'.wq th{cursor:pointer;white-space:nowrap}.wq .scroll{overflow-x:auto}.wq input,.wq select{flex:1;min-width:0}';
  var $=function(tag,attrs,kids){var n=document.createElement(tag);for(var k in attrs||{}){if(k==='text')n.textContent=attrs[k];else if(k==='class')n.className=attrs[k];else n.setAttribute(k,attrs[k])}(kids||[]).forEach(function(c){n.appendChild(c)});return n};
  var num=function(v,d){var x=Number(v);return isFinite(x)?x:d};
  var fmt=function(v){return String(Math.round(v*1e9)/1e9).replace('.',',')};
  var load=function(k){try{return window.wiqo.load(k)}catch(e){return null}};
  var save=function(k,v){try{window.wiqo.save(k,v)}catch(e){}};
  var haptic=function(k){try{window.wiqo.haptic(k)}catch(e){}};

  function checklist(root,c){
    var items=(c.items||[]).map(function(it){return typeof it==='string'?{text:it,done:false}:{text:String(it.text||''),done:!!it.done}});
    var stored;try{stored=JSON.parse(load('done')||'null')}catch(e){stored=null}
    if(stored&&stored.length===items.length)items.forEach(function(it,i){it.done=!!stored[i]});
    var bar=$('div',{class:'bar'},[$('i')]),count=$('div',{class:'muted'}),list=$('div');
    function draw(){
      list.innerHTML='';var d=0;
      items.forEach(function(it,i){
        if(it.done)d++;
        var cb=$('input',{type:'checkbox'});cb.checked=it.done;
        cb.addEventListener('change',function(){it.done=cb.checked;save('done',JSON.stringify(items.map(function(x){return x.done?1:0})));haptic('light');draw();if(d+(it.done?1:-1)===items.length&&it.done&&window.WiqoConfetti)window.WiqoConfetti.burst()});
        var row=$('label',{class:'item'+(it.done?' done':'')},[cb,$('span',{text:it.text})]);list.appendChild(row);
      });
      bar.firstChild.style.width=(items.length?d/items.length*100:0)+'%';count.textContent=d+' / '+items.length+' tamamlandı';
    }
    root.appendChild(bar);root.appendChild(count);root.appendChild(list);draw();
  }
  function counter(root,c){
    var step=num(c.step,1),goal=c.goal===undefined?null:num(c.goal,null),start=num(c.start,0);
    var v=num(load('value'),start);var out=$('div',{class:'big'}),bar=$('div',{class:'bar'},[$('i')]);
    function draw(){out.textContent=fmt(v);if(goal){bar.firstChild.style.width=Math.max(0,Math.min(100,v/goal*100))+'%'}save('value',String(v))}
    function change(d){v=Math.round((v+d)*1e9)/1e9;haptic('light');draw();if(goal&&v>=goal&&window.WiqoConfetti)window.WiqoConfetti.burst()}
    var minus=$('button',{text:'−','aria-label':'Azalt'}),plus=$('button',{text:'+','aria-label':'Artır'}),reset=$('button',{text:'Sıfırla',class:'secondary'});
    minus.onclick=function(){change(-step)};plus.onclick=function(){change(step)};reset.onclick=function(){v=start;draw()};
    root.appendChild(out);if(goal){root.appendChild(bar);root.appendChild($('div',{class:'muted',text:'Hedef: '+fmt(goal)}))}
    root.appendChild($('div',{class:'row',style:'justify-content:center'},[minus,plus,reset]));draw();
  }
  function timer(root,c){
    var down=c.mode!=='stopwatch',total=Math.max(1,Math.round(num(c.seconds,60))),left=down?total:0,run=false,last=0,id=null;
    var out=$('div',{class:'big'}),go=$('button',{text:'Başlat'}),reset=$('button',{text:'Sıfırla',class:'secondary'});
    function show(){var s=Math.max(0,Math.ceil(down?left:left)),m=Math.floor(s/60),r=s%60,h=Math.floor(m/60);out.textContent=(h?h+':':'')+('0'+(h?m%60:m)).slice(-2)+':'+('0'+r).slice(-2)}
    function tick(){var now=Date.now(),d=(now-last)/1000;last=now;if(down){left-=d;if(left<=0){left=0;stop();haptic('success');go.textContent='Başlat';if(window.WiqoConfetti)window.WiqoConfetti.burst()}}else left+=d;show()}
    function stop(){run=false;clearInterval(id);id=null;go.textContent='Başlat'}
    go.onclick=function(){if(run){stop();go.textContent='Devam'}else{if(down&&left<=0)left=total;run=true;last=Date.now();id=setInterval(tick,200);go.textContent='Duraklat'}};
    reset.onclick=function(){stop();left=down?total:0;show()};
    root.appendChild(out);root.appendChild($('div',{class:'row',style:'justify-content:center'},[go,reset]));show();
  }
  function evaluate(src){
    var t=src.replace(/×/g,'*').replace(/÷/g,'/').replace(/,/g,'.').replace(/−/g,'-'),pos=0;
    function ws(){while(t[pos]===' ')pos++}
    function number(){ws();var s=pos;if(t[pos]==='('){pos++;var v=expr();ws();if(t[pos]!==')')throw 0;pos++;return v}
      if(t[pos]==='-'){pos++;return -number()}
      while(/[0-9.]/.test(t[pos]||''))pos++;if(s===pos)throw 0;var x=parseFloat(t.slice(s,pos));ws();if(t[pos]==='%'){pos++;x/=100}return x}
    function term(){var v=number();for(;;){ws();var o=t[pos];if(o==='*'||o==='/'){pos++;var r=number();v=o==='*'?v*r:v/r}else return v}}
    function expr(){var v=term();for(;;){ws();var o=t[pos];if(o==='+'||o==='-'){pos++;var r=term();v=o==='+'?v+r:v-r}else return v}}
    var r=expr();ws();if(pos<t.length)throw 0;return r;
  }
  function calculator(root){
    var scr=$('div',{class:'screen',text:'0'}),grid=$('div',{class:'grid'}),exp='';
    ['C','(',')','÷','7','8','9','×','4','5','6','−','1','2','3','+','0',',','⌫','='].forEach(function(k){
      var b=$('button',{text:k,class:/[0-9,]/.test(k)?'secondary':''});
      b.onclick=function(){
        haptic('light');
        if(k==='C')exp='';else if(k==='⌫')exp=exp.slice(0,-1);
        else if(k==='='){try{var r=evaluate(exp);if(!isFinite(r))throw 0;exp=fmt(r)}catch(e){exp='';scr.textContent='Hata';return}}
        else exp+=k;
        scr.textContent=exp||'0';
      };grid.appendChild(b);
    });
    root.appendChild(scr);root.appendChild(grid);
  }
  function converter(root,c){
    var units=(c.units||[]).map(function(u){return{name:String(u.name),factor:num(u.factor,1)||1,offset:num(u.offset,0)}});
    if(units.length<2){root.textContent='En az iki birim gerekli.';return}
    var input=$('input',{type:'number',inputmode:'decimal',value:String(num(c.value,1))}),from=$('select'),to=$('select'),out=$('div',{class:'big'});
    units.forEach(function(u,i){from.appendChild($('option',{value:String(i),text:u.name}));to.appendChild($('option',{value:String(i),text:u.name}))});to.value='1';
    function calc(){var a=units[+from.value],b=units[+to.value],v=parseFloat(input.value);if(!isFinite(v)){out.textContent='—';return}var base=v*a.factor+a.offset;out.textContent=fmt((base-b.offset)/b.factor)+' '+b.name}
    [input,from,to].forEach(function(n){n.addEventListener('input',calc);n.addEventListener('change',calc)});
    root.appendChild($('div',{class:'row'},[input,from]));root.appendChild($('div',{class:'row',style:'margin-top:8px'},[to]));root.appendChild(out);calc();
  }
  function quiz(root,c){
    var qs=(c.questions||[]).filter(function(q){return q&&q.q&&q.options&&q.options.length>1});
    if(!qs.length){root.textContent='Soru yok.';return}
    var i=0,score=0,answered=false,box=$('div');root.appendChild(box);
    function ask(){
      answered=false;box.innerHTML='';var q=qs[i];
      box.appendChild($('div',{class:'muted',text:'Soru '+(i+1)+' / '+qs.length}));box.appendChild($('h2',{text:q.q}));
      var note=$('div',{class:'muted',style:'margin-top:8px'}),next=$('button',{text:i+1<qs.length?'Sonraki':'Sonucu gör',style:'display:none;margin-top:10px'});
      q.options.forEach(function(o,oi){
        var b=$('button',{class:'secondary opt',text:String(o)});
        b.onclick=function(){
          if(answered)return;answered=true;var right=oi===num(q.answer,0);if(right){score++;b.className='opt ok';haptic('success')}else{b.className='opt bad';haptic('error');var ob=box.querySelectorAll('.opt')[num(q.answer,0)];if(ob)ob.className='opt ok'}
          if(q.explain)note.textContent=String(q.explain);next.style.display='inline-block';
        };box.appendChild(b);
      });
      box.appendChild(note);box.appendChild(next);next.onclick=function(){i++;if(i<qs.length)ask();else done()};
    }
    function done(){
      box.innerHTML='';box.appendChild($('div',{class:'big',text:score+' / '+qs.length}));
      box.appendChild($('div',{class:'muted',style:'text-align:center',text:score===qs.length?'Hepsi doğru!':score>=qs.length/2?'İyi gidiyorsun.':'Bir tur daha dene.'}));
      var again=$('button',{text:'Tekrar',style:'display:block;margin:12px auto 0'});again.onclick=function(){i=0;score=0;ask()};box.appendChild(again);
      if(score===qs.length&&window.WiqoConfetti)window.WiqoConfetti.burst();
    }
    ask();
  }
  function table(root,c){
    var cols=(c.columns||[]).map(String),rows=(c.rows||[]).map(function(r){return(r||[]).map(function(x){return x===null||x===undefined?'':String(x)})});
    var sort=-1,asc=true,q='',wrap=$('div',{class:'scroll'});
    var filter=rows.length>8?$('input',{type:'search',placeholder:'Tabloda ara'}):null;
    function draw(){
      var list=rows.filter(function(r){return!q||r.some(function(x){return x.toLowerCase().indexOf(q)>=0})});
      if(sort>=0)list.sort(function(a,b){var x=a[sort]||'',y=b[sort]||'',nx=parseFloat(x.replace(/[^0-9,.\-]/g,'').replace(',','.')),ny=parseFloat(y.replace(/[^0-9,.\-]/g,'').replace(',','.'));var r=isFinite(nx)&&isFinite(ny)?nx-ny:x.localeCompare(y,'tr');return asc?r:-r});
      var t=$('table'),hr=$('tr');
      cols.forEach(function(h,i){var th=$('th',{text:h+(sort===i?(asc?' ▲':' ▼'):'')});th.onclick=function(){if(sort===i)asc=!asc;else{sort=i;asc=true}draw()};hr.appendChild(th)});
      t.appendChild($('thead',{},[hr]));var tb=$('tbody');list.forEach(function(r){var tr=$('tr');cols.forEach(function(_,i){tr.appendChild($('td',{text:r[i]||''}))});tb.appendChild(tr)});t.appendChild(tb);
      wrap.innerHTML='';wrap.appendChild(t);
    }
    if(filter){filter.addEventListener('input',function(){q=filter.value.toLowerCase();draw()});root.appendChild(filter)}
    root.appendChild(wrap);draw();
  }
  function chart(root,c){var host=$('div');root.appendChild(host);window.WiqoChart.render(host,c)}

  var KINDS={checklist:checklist,counter:counter,timer:timer,calculator:calculator,converter:converter,quiz:quiz,table:table,chart:chart};
  function mount(root,config){
    var c=config||window.WIQO_DATA||{};var fn=KINDS[c.kind];
    if(!document.getElementById('wq-css')){var s=document.createElement('style');s.id='wq-css';s.textContent=css;document.head.appendChild(s)}
    root.className='wq';root.innerHTML='';
    if(c.title)root.appendChild($('h2',{text:String(c.title)}));
    if(!fn){root.appendChild($('div',{class:'muted',text:'Bu araç türü bilinmiyor.'}));return}
    fn(root,c);
  }
  window.WiqoTemplates=Object.freeze({mount:mount,kinds:Object.keys(KINDS),evaluate:evaluate});
})();
