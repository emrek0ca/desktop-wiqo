(function(){
  var NS='http://www.w3.org/2000/svg';
  var COLORS=['#0a84ff','#30b0c7','#ff9f0a','#bf5af2','#32d74b','#ff453a','#64d2ff','#ffd60a'];
  function el(name,attrs,text){var n=document.createElementNS(NS,name);for(var k in attrs)n.setAttribute(k,attrs[k]);if(text!==undefined)n.textContent=text;return n}
  function nice(max){if(max<=0)return 1;var p=Math.pow(10,Math.floor(Math.log10(max)));var f=max/p;return(f<=1?1:f<=2?2:f<=5?5:10)*p}
  function fmt(v){var a=Math.abs(v);if(a>=1e9)return(v/1e9).toFixed(1).replace(/\.0$/,'')+' Mr';if(a>=1e6)return(v/1e6).toFixed(1).replace(/\.0$/,'')+' Mn';if(a>=1e4)return(v/1e3).toFixed(0)+' B';return String(Math.round(v*100)/100).replace('.',',')}
  function series(o){
    if(o.series&&o.series.length)return o.series.map(function(s,i){return{name:String(s.name||('Seri '+(i+1))),values:(s.values||[]).map(Number)}});
    return[{name:String(o.title||''),values:(o.values||[]).map(Number)}];
  }
  /* WiqoChart.render(container, {type:'bar'|'line'|'pie', labels:[], values:[] | series:[{name,values}], unit?, title?}) */
  function render(root,o){
    o=o||{};var labels=(o.labels||[]).map(String);var data=series(o);var type=o.type||'bar';
    var n=Math.max(labels.length,data[0]?data[0].values.length:0);
    root.innerHTML='';
    if(!n||!data[0]||!data[0].values.length){root.textContent='Gösterilecek veri yok.';return}
    var W=360,H=type==='pie'?240:220,svg=el('svg',{viewBox:'0 0 '+W+' '+H,role:'img','aria-label':String(o.title||'Grafik'),style:'width:100%;height:auto;display:block;font:11px -apple-system,system-ui,sans-serif'});
    var fg='var(--fg,#14161a)',muted='var(--muted,#5c6573)',grid='var(--border,rgba(120,120,128,.28))';
    if(type==='pie'){
      var vals=data[0].values,total=vals.reduce(function(a,b){return a+Math.max(0,b)},0)||1,cx=110,cy=120,r=90,a0=-Math.PI/2;
      vals.forEach(function(v,i){
        var a1=a0+Math.max(0,v)/total*Math.PI*2;
        if(Math.max(0,v)/total>=0.9999){svg.appendChild(el('circle',{cx:cx,cy:cy,r:r,fill:COLORS[i%COLORS.length]}))}
        else{var large=a1-a0>Math.PI?1:0;svg.appendChild(el('path',{d:'M'+cx+','+cy+' L'+(cx+r*Math.cos(a0))+','+(cy+r*Math.sin(a0))+' A'+r+','+r+' 0 '+large+' 1 '+(cx+r*Math.cos(a1))+','+(cy+r*Math.sin(a1))+' Z',fill:COLORS[i%COLORS.length],stroke:'var(--bg,#fff)','stroke-width':1.5}))}
        a0=a1;
      });
      vals.slice(0,10).forEach(function(v,i){
        var y=30+i*20;svg.appendChild(el('rect',{x:225,y:y-9,width:10,height:10,rx:2,fill:COLORS[i%COLORS.length]}));
        svg.appendChild(el('text',{x:241,y:y,fill:fg},(labels[i]||('#'+(i+1)))+' · %'+Math.round(Math.max(0,v)/total*100)));
      });
      root.appendChild(svg);return;
    }
    var all=[];data.forEach(function(s){all=all.concat(s.values)});
    var min=Math.min(0,Math.min.apply(null,all)),max=nice(Math.max.apply(null,all.concat([0.0001])));
    var L=38,R=8,T=14,B=labels.length>6?44:28,pw=W-L-R,ph=H-T-B,span=max-min||1;
    var y=function(v){return T+ph-(v-min)/span*ph};
    for(var g=0;g<=4;g++){var gv=min+span*g/4,gy=y(gv);svg.appendChild(el('line',{x1:L,x2:W-R,y1:gy,y2:gy,stroke:grid,'stroke-width':.6}));svg.appendChild(el('text',{x:L-4,y:gy+3,'text-anchor':'end',fill:muted},fmt(gv)))}
    var step=pw/n;
    labels.slice(0,n).forEach(function(lab,i){
      var x=L+step*(i+.5),t=el('text',{x:x,y:H-B+14,'text-anchor':labels.length>6?'end':'middle',fill:muted,transform:labels.length>6?'rotate(-35 '+x+' '+(H-B+14)+')':''},lab.length>12?lab.slice(0,11)+'…':lab);
      if(labels.length<=12||i%Math.ceil(labels.length/12)===0)svg.appendChild(t);
    });
    if(type==='line'){
      data.forEach(function(s,si){
        var pts=s.values.map(function(v,i){return[L+step*(i+.5),y(v)]});
        svg.appendChild(el('polyline',{points:pts.map(function(p){return p.join(',')}).join(' '),fill:'none',stroke:COLORS[si%COLORS.length],'stroke-width':2.2,'stroke-linejoin':'round','stroke-linecap':'round'}));
        pts.forEach(function(p,i){svg.appendChild(el('circle',{cx:p[0],cy:p[1],r:2.8,fill:COLORS[si%COLORS.length]}));if(n<=10&&data.length===1)svg.appendChild(el('text',{x:p[0],y:p[1]-6,'text-anchor':'middle',fill:fg},fmt(s.values[i])))});
      });
    }else{
      var gw=step*.72,bw=gw/data.length;
      data.forEach(function(s,si){s.values.forEach(function(v,i){
        var x=L+step*i+(step-gw)/2+bw*si,top=y(Math.max(v,0)),h=Math.abs(y(v)-y(0));
        svg.appendChild(el('rect',{x:x,y:v>=0?top:y(0),width:Math.max(1,bw-1),height:Math.max(1,h),rx:3,fill:COLORS[si%COLORS.length]}));
        if(n<=10&&data.length===1)svg.appendChild(el('text',{x:x+bw/2,y:(v>=0?top:y(v))-4,'text-anchor':'middle',fill:fg},fmt(v)+(o.unit?' '+o.unit:'')));
      })});
    }
    root.appendChild(svg);
    if(data.length>1){var lg=document.createElement('div');lg.style.cssText='display:flex;gap:12px;flex-wrap:wrap;font-size:12px;margin-top:6px';data.forEach(function(s,i){var it=document.createElement('span');it.innerHTML='<i style="display:inline-block;width:10px;height:10px;border-radius:2px;margin-right:5px;background:'+COLORS[i%COLORS.length]+'"></i>';it.appendChild(document.createTextNode(s.name));lg.appendChild(it)});root.appendChild(lg)}
  }
  window.WiqoChart=Object.freeze({render:render});
})();
